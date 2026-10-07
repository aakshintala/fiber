//! Tests for raw mode, the alternate screen and restore, on a pty pair.

use super::{restore, setup, size};
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// What `setup` writes with hover on: the alternate screen, bracketed
/// paste, mouse modes 1000, 1002, 1006 and 1003, then the two queries.
const START_HOVER: &[u8] =
    b"\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[?u\x1b[c";
/// What `setup` writes with hover off: no mode 1003.
const START_NO_HOVER: &[u8] =
    b"\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?u\x1b[c";
/// What `restore` writes: kitty's flags popped, bracketed paste and every
/// mouse mode off, the alternate screen left, the cursor shown.
const END: &[u8] =
    b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h";

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// A pty pair: the main side and the slave as a file.
struct Pair {
    /// The main side, for reading what the slave writes.
    main: File,
    /// The slave side, the injected tty.
    slave: File,
}

/// Opens a pty pair.
fn open() -> Pair {
    let main =
        rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
            .unwrap_or_else(|err| panic!("openpt: {err}"));
    rustix::pty::grantpt(&main).unwrap_or_else(|err| panic!("grantpt: {err}"));
    rustix::pty::unlockpt(&main).unwrap_or_else(|err| panic!("unlockpt: {err}"));
    let name =
        rustix::pty::ptsname(&main, Vec::new()).unwrap_or_else(|err| panic!("ptsname: {err}"));
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(name.as_bytes()));
    let slave = File::options()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|err| panic!("open slave: {err}"));
    let main = File::from(main);
    Pair { main, slave }
}

/// Reads exactly `n` bytes with one named deadline.
fn read_exact(main: &File, n: usize, what: &str) -> Vec<u8> {
    let mut dup = main.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("term-read".to_owned())
        .spawn(move || {
            let mut buf = vec![0u8; n];
            let read = dup.read_exact(&mut buf).map(|()| buf);
            match done.send(read) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match finished.recv_timeout(DEADLINE) {
        Ok(Ok(buf)) => buf,
        Ok(Err(err)) => panic!("waited {DEADLINE:?} for {what}: {err}"),
        Err(_) => panic!("waited {DEADLINE:?} for {what}"),
    }
}

#[test]
fn restore_before_setup_does_nothing() {
    restore();
}

#[test]
fn setup_writes_alt_screen_mouse_modes_then_queries() {
    let pair = open();
    setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let bytes = read_exact(&pair.main, START_HOVER.len(), "the start bytes");
    assert_eq!(bytes, START_HOVER);
    restore();
    assert_eq!(read_exact(&pair.main, END.len(), "the restore bytes"), END);
}

#[test]
fn setup_without_hover_drops_mode_1003_and_restore_still_turns_it_off() {
    let pair = open();
    setup(&pair.slave, false).unwrap_or_else(|err| panic!("setup: {err}"));
    let bytes = read_exact(&pair.main, START_NO_HOVER.len(), "the start bytes");
    assert_eq!(bytes, START_NO_HOVER);
    restore();
    assert_eq!(read_exact(&pair.main, END.len(), "the restore bytes"), END);
}

#[test]
fn setup_sets_raw_mode_and_restore_puts_it_back() {
    let pair = open();
    let before =
        rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let _ = read_exact(&pair.main, START_HOVER.len(), "the start bytes");
    let raw = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&before));
    assert!(!is_cooked(&raw));
    restore();
    let _ = read_exact(&pair.main, END.len(), "the restore bytes");
    let after = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&after));
}

/// Whether canonical mode, echo and signals are on. Compared flag by flag:
/// the kernel may set `PENDIN` on its own, so a whole-struct equality would
/// be brittle.
fn is_cooked(termios: &rustix::termios::Termios) -> bool {
    use rustix::termios::LocalModes;
    termios.local_modes.contains(LocalModes::ICANON)
        && termios.local_modes.contains(LocalModes::ECHO)
        && termios.local_modes.contains(LocalModes::ISIG)
}

#[test]
fn size_reads_the_winsize() {
    let pair = open();
    let winsize = rustix::termios::Winsize {
        ws_col: 60,
        ws_row: 12,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    rustix::termios::tcsetwinsize(&pair.slave, winsize)
        .unwrap_or_else(|err| panic!("winsize: {err}"));
    assert_eq!(
        size(&pair.slave).unwrap_or_else(|err| panic!("size: {err}")),
        (60, 12)
    );
}
