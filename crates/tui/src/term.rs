//! The injected tty: raw mode, the alternate screen, mouse reporting, the
//! detection queries, the size, and the restore (`docs/tui.md`, "Keys",
//! "Mouse and hover").

use std::fs::File;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::termios::{self, OptionalActions, Termios};

use crate::appearance;

/// Enters the alternate screen.
const ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";
/// Pushes the window title, so the restore puts the person's back
/// (`docs/tui.md`, "State glyphs").
const PUSH_TITLE: &[u8] = b"\x1b[22;2t";
/// Turns bracketed paste on.
const BRACKETED_PASTE: &[u8] = b"\x1b[?2004h";
/// Mouse reporting: presses and releases (1000), drags (1002), in SGR form
/// (1006).
const MOUSE: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
/// Focus reporting (1004), for the Quota card.
const FOCUS: &[u8] = b"\x1b[?1004h";
/// Mouse reporting of every motion (1003), for hover.
const MOTION: &[u8] = b"\x1b[?1003h";
/// Kitty's graphics probe id: the query image's id, which the image
/// registry never hands out (`docs/tui.md`, "Images").
pub(crate) const PROBE_ID: u32 = 31;

/// Kitty's keyboard flags query, then the primary device attributes query,
/// then kitty's graphics probe: a one-pixel query image under
/// [`PROBE_ID`]. The appearance queries go first, so the theme follows
/// the terminal before detection ends (`docs/tui.md`, "Themes").
const QUERIES: &[u8] = b"\x1b[?u\x1b[c\x1b_Gi=31,a=q,s=1,v=1,f=24,m=0;AAAA\x1b\\";
/// Pushes kitty's keyboard flag 1, disambiguate escape codes. The loop
/// writes it when kitty's flags reply arrives.
pub(crate) const KITTY_PUSH: &[u8] = b"\x1b[>1u";
/// Pops kitty's keyboard flags while still on the alternate screen, turns
/// bracketed paste off, turns every mouse mode off, whichever were on,
/// resets the pointer shape to the default, pops the window title, then
/// leaves the alternate screen and shows the cursor.
const RESTORE: &[u8] =
    b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1004l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";

/// The tty `setup` changed and its modes from before. One terminal per
/// process: the first `setup` records it.
static SAVED: OnceLock<(File, Termios)> = OnceLock::new();
/// Whether the terminal is set up and not yet restored.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Sets `tty` up: raw mode, the alternate screen, the window title pushed,
/// bracketed paste, mouse
/// reporting (with every motion only when `hover`), then the two queries.
/// Returns its size in columns and rows.
pub(crate) fn setup(mut tty: &File, hover: bool) -> io::Result<(u16, u16)> {
    let saved = termios::tcgetattr(tty)?;
    let restore = tty.try_clone()?;
    let mut raw = saved.clone();
    raw.make_raw();
    if SAVED.set((restore, saved)).is_err() {
        return Err(io::Error::other("the terminal is already set up"));
    }
    ACTIVE.store(true, Ordering::SeqCst);
    termios::tcsetattr(tty, OptionalActions::Now, &raw)?;
    tty.write_all(ALTERNATE_SCREEN)?;
    tty.write_all(PUSH_TITLE)?;
    tty.write_all(BRACKETED_PASTE)?;
    tty.write_all(MOUSE)?;
    tty.write_all(FOCUS)?;
    if hover {
        tty.write_all(MOTION)?;
    }
    tty.write_all(appearance::QUERIES)?;
    tty.write_all(QUERIES)?;
    tty.flush()?;
    size(tty)
}

/// The size of `tty` in columns and rows, at least 1 by 1.
pub(crate) fn size(tty: &File) -> io::Result<(u16, u16)> {
    let size = termios::tcgetwinsize(tty)?;
    Ok((size.ws_col.max(1), size.ws_row.max(1)))
}

/// One cell's size in pixels from the tty's pixel size over its cell
/// counts, for the placeholder grid (`docs/tui.md`, "Images"). Zero
/// pixels or a zero count reads the (10, 20) fallback, so the grid math
/// never divides by zero.
#[allow(
    dead_code,
    reason = "no production caller until the image sizing wires it"
)]
pub(crate) fn cell(pixel: (u16, u16), count: (u16, u16)) -> (u16, u16) {
    let (wide, high) = pixel;
    let (cols, rows) = count;
    if wide == 0 || high == 0 || cols == 0 || rows == 0 {
        return (10, 20);
    }
    ((wide / cols).max(1), (high / rows).max(1))
}

/// Restores the terminal `setup` set up, once: pops kitty's keyboard
/// flags, turns bracketed paste and mouse reporting off, pops the window
/// title, leaves the alternate screen, shows the cursor and puts the saved modes back. Does
/// nothing
/// before `setup` or after the first restore.
pub(crate) fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let Some((tty, saved)) = SAVED.get() else {
        return;
    };
    let mut out: &File = tty;
    out.write_all(RESTORE)
        .and_then(|()| out.flush())
        .unwrap_or(());
    termios::tcsetattr(tty, OptionalActions::Now, saved).unwrap_or(());
}

/// Hands the terminal back for a program run in the foreground, such as the
/// editor: writes the restore bytes and puts the saved modes back, as
/// [`restore`] does. [`restore`] after it writes nothing more. Does nothing
/// before `setup` or while already restored.
pub(crate) fn suspend() -> io::Result<()> {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    let Some((tty, saved)) = SAVED.get() else {
        return Ok(());
    };
    let mut out: &File = tty;
    out.write_all(RESTORE)?;
    out.flush()?;
    termios::tcsetattr(tty, OptionalActions::Now, saved)?;
    Ok(())
}

/// Takes the terminal back after [`suspend`]: raw mode, the alternate
/// screen, the title pushed again, bracketed paste and mouse reporting as `setup` turned them on
/// (every motion only when `hover`), and kitty's flags pushed again when
/// `kitty` says they were. Fails before `setup`, or when the tty is gone.
pub(crate) fn resume(kitty: bool, hover: bool) -> io::Result<()> {
    let (tty, saved) = SAVED
        .get()
        .ok_or_else(|| io::Error::other("the terminal was never set up"))?;
    let mut raw = saved.clone();
    raw.make_raw();
    ACTIVE.store(true, Ordering::SeqCst);
    termios::tcsetattr(tty, OptionalActions::Now, &raw)?;
    let mut out: &File = tty;
    out.write_all(ALTERNATE_SCREEN)?;
    out.write_all(PUSH_TITLE)?;
    out.write_all(BRACKETED_PASTE)?;
    out.write_all(MOUSE)?;
    out.write_all(FOCUS)?;
    if hover {
        out.write_all(MOTION)?;
    }
    if kitty {
        out.write_all(KITTY_PUSH)?;
    }
    // The appearance queries go last, after everything else resumed, so
    // the theme follows the terminal again (`docs/tui.md`, "Themes").
    out.write_all(appearance::QUERIES)?;
    out.flush()
}

/// Restores the terminal when dropped, on every return from `run`.
pub(crate) struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

#[cfg(test)]
#[path = "term_tests.rs"]
mod tests;
