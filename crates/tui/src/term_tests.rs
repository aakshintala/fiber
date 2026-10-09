//! Tests for raw mode, the alternate screen and restore, on a pty pair.

use super::{restore, setup, size};
use crate::pty_watch::{watch, watched};
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// What `setup` writes with hover on: the alternate screen, the title
/// pushed, bracketed paste, mouse modes 1000, 1002, 1006 and 1003, the
/// appearance queries, then the two detection queries.
const START_HOVER: &[u8] =
    b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
/// What `setup` writes with hover off: no mode 1003.
const START_NO_HOVER: &[u8] =
    b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
/// What `restore` writes: kitty's flags popped, bracketed paste and every
/// mouse mode off, theme reporting off, the title popped, the alternate
/// screen left, the cursor shown.
const END: &[u8] =
    b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";

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

#[test]
fn restore_before_setup_does_nothing() {
    restore();
}

#[test]
fn setup_writes_alt_screen_mouse_modes_then_queries() {
    let pair = open();
    let frames = watch(&pair.main, vec![START_HOVER, END]);
    setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    assert_eq!(watched(&frames, "the start bytes"), START_HOVER);
    restore();
    assert_eq!(watched(&frames, "the restore bytes"), END);
}

#[test]
fn setup_without_hover_drops_mode_1003_and_restore_still_turns_it_off() {
    let pair = open();
    let frames = watch(&pair.main, vec![START_NO_HOVER, END]);
    setup(&pair.slave, false).unwrap_or_else(|err| panic!("setup: {err}"));
    assert_eq!(watched(&frames, "the start bytes"), START_NO_HOVER);
    restore();
    assert_eq!(watched(&frames, "the restore bytes"), END);
}

#[test]
fn setup_sets_raw_mode_and_restore_puts_it_back() {
    let pair = open();
    let frames = watch(&pair.main, vec![START_HOVER, END]);
    let before =
        rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    assert_eq!(watched(&frames, "the start bytes"), START_HOVER);
    let raw = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&before));
    assert!(!is_cooked(&raw));
    restore();
    assert_eq!(watched(&frames, "the restore bytes"), END);
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

/// What `resume` writes before kitty's push: as `setup` without the
/// queries.
const RESUME_NO_HOVER: &[u8] = b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h";

#[test]
fn suspend_restores_and_resume_sets_up_again_without_hover_or_kitty() {
    let pair = open();
    let frames = watch(&pair.main, vec![START_NO_HOVER, END, b"mark" as &[u8], END]);
    setup(&pair.slave, false).unwrap_or_else(|err| panic!("setup: {err}"));
    assert_eq!(watched(&frames, "the start bytes"), START_NO_HOVER);
    super::suspend().unwrap_or_else(|err| panic!("suspend: {err}"));
    assert_eq!(watched(&frames, "the suspend bytes"), END);
    let cooked =
        rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&cooked));
    super::resume(false, false).unwrap_or_else(|err| panic!("resume: {err}"));
    let raw = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(!is_cooked(&raw));
    // No 1003 and no kitty push, then the appearance queries: the next
    // bytes are the test's own.
    (&pair.slave)
        .write_all(b"mark")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let expected = [RESUME_NO_HOVER, crate::appearance::QUERIES, b"mark"].concat();
    assert_eq!(watched(&frames, "the resume bytes"), expected);
    // Set up again: restore writes the restore bytes once more.
    restore();
    assert_eq!(watched(&frames, "the restore bytes"), END);
}

#[test]
fn resume_turns_hover_on_and_pushes_kitty_when_they_were() {
    let pair = open();
    let frames = watch(&pair.main, vec![START_HOVER, END, b"mark" as &[u8]]);
    setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    assert_eq!(watched(&frames, "the start bytes"), START_HOVER);
    super::suspend().unwrap_or_else(|err| panic!("suspend: {err}"));
    assert_eq!(watched(&frames, "the suspend bytes"), END);
    super::resume(true, true).unwrap_or_else(|err| panic!("resume: {err}"));
    // No 1003 and no kitty push, then the appearance queries: the next
    // bytes are the test's own.
    (&pair.slave)
        .write_all(b"mark")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let expected = [
        RESUME_NO_HOVER,
        b"\x1b[?1003h\x1b[>1u",
        crate::appearance::QUERIES,
        b"mark",
    ]
    .concat();
    assert_eq!(watched(&frames, "the resume bytes"), expected);
    restore();
}

#[test]
fn restore_after_a_suspend_writes_nothing_more() {
    let pair = open();
    let frames = watch(&pair.main, vec![START_HOVER, END, b"mark" as &[u8]]);
    setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    assert_eq!(watched(&frames, "the start bytes"), START_HOVER);
    super::suspend().unwrap_or_else(|err| panic!("suspend: {err}"));
    assert_eq!(watched(&frames, "the suspend bytes"), END);
    restore();
    // A second suspend does nothing either.
    super::suspend().unwrap_or_else(|err| panic!("suspend: {err}"));
    (&pair.slave)
        .write_all(b"mark")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(watched(&frames, "the mark"), b"mark");
}

#[test]
fn resume_before_setup_fails() {
    assert!(super::resume(false, false).is_err());
}

#[test]
fn the_restore_turns_scheme_reports_off_before_the_title_pops() {
    let off = b"\x1b[?2031l";
    let pop = b"\x1b[23;2t";
    let Some(at) = super::RESTORE
        .windows(off.len())
        .position(|window| window == off)
    else {
        panic!("the restore turns scheme reports off");
    };
    let Some(title) = super::RESTORE
        .windows(pop.len())
        .position(|window| window == pop)
    else {
        panic!("the restore pops the title");
    };
    assert!(
        at + off.len() <= title,
        "scheme reports off before the title pops"
    );
}

#[test]
fn the_restore_bytes_pop_the_title_pushed_on_setup_and_resume() {
    let pop = b"\x1b[23;2t";
    let at = super::RESTORE
        .windows(pop.len())
        .position(|window| window == pop);
    assert!(at.is_some(), "the restore pops the title");
    assert_eq!(super::PUSH_TITLE, b"\x1b[22;2t");
    assert!(START_HOVER.starts_with(b"\x1b[?1049h\x1b[22;2t"));
    assert!(RESUME_NO_HOVER.starts_with(b"\x1b[?1049h\x1b[22;2t"));
}

#[test]
fn restore_resets_the_pointer_shape() {
    let pointer = crate::osc::pointer(false);
    let pop = b"\x1b[23;2t";
    let shape = super::RESTORE
        .windows(pointer.len())
        .position(|window| window == pointer);
    let title = super::RESTORE
        .windows(pop.len())
        .position(|window| window == pop);
    assert!(shape.is_some(), "the restore resets the pointer shape");
    assert!(shape < title, "the shape resets before the title pops");
}
