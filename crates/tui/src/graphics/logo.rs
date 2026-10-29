//! Home's logo image: a tinted wave and name where the terminal speaks
//! an image protocol, in exactly the pixel logo's cells (`docs/tui.md`,
//! "The logo"). The mask is checked in, built from the font with
//! `cargo xtask logo-mask`; the tint comes from the theme at run time,
//! so Fiber bundles no font and the logo follows any theme.

use crate::keys::{Event, Reply};

/// The logo's own image id: skipped by the image registry exactly like
/// the graphics probe's, so no ordinary image part ever collides with
/// the logo's transmission (`docs/tui.md`, "The logo").
pub(crate) const ID: u32 = 30;

/// One XTVERSION reply candidate: `buf` starts with `ESC P`.
#[derive(Debug)]
pub(crate) enum Dcs {
    /// Every byte so far fits the grammar; the rest may follow.
    Hold,
    /// A complete reply: its events and the reply's length.
    Done(Vec<Event>, usize),
    /// The first byte off the grammar: read `ESC P` as keys instead.
    Off,
}

/// The longest a held XTVERSION reply grows: a version line is short,
/// so anything longer is typed text, and resolving it keeps the parser
/// from holding a never-terminated candidate forever.
const DCS_CAP: usize = 256;

/// Reads an XTVERSION reply byte by byte: `ESC P > | <text>` with `ESC \`
/// or BEL after it. A buffer that ends while every byte so far fits is
/// held. The first byte that cannot continue the grammar ends the
/// candidate, so a typed Alt+Shift+P followed by text stays keys.
pub(crate) fn dcs(buf: &[u8]) -> Dcs {
    if buf.len() > DCS_CAP {
        return Dcs::Off;
    }
    const PREFIX: &[u8] = b"\x1bP>|";
    for (at, want) in PREFIX.iter().enumerate() {
        match buf.get(at) {
            None => return Dcs::Hold,
            Some(byte) if byte == want => {}
            Some(_) => return Dcs::Off,
        }
    }
    let mut at = PREFIX.len();
    loop {
        match buf.get(at) {
            None => return Dcs::Hold,
            Some(0x07) => {
                let text = String::from_utf8_lossy(buf.get(PREFIX.len()..at).unwrap_or_default())
                    .into_owned();
                return Dcs::Done(vec![Event::Reply(Reply::Version(text))], at + 1);
            }
            Some(0x1b) => match buf.get(at.saturating_add(1)) {
                None => return Dcs::Hold,
                Some(b'\\') => {
                    let text =
                        String::from_utf8_lossy(buf.get(PREFIX.len()..at).unwrap_or_default())
                            .into_owned();
                    return Dcs::Done(
                        vec![Event::Reply(Reply::Version(text))],
                        at.saturating_add(2),
                    );
                }
                Some(_) => return Dcs::Off,
            },
            // A control byte is typed text, not a version.
            Some(byte) if *byte < 0x20 => return Dcs::Off,
            Some(_) => at = at.saturating_add(1),
        }
    }
}

#[cfg(test)]
#[path = "logo_tests.rs"]
mod tests;
