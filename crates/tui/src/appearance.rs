//! The terminal's light or dark appearance: the queries that ask for it
//! and the replies that report it (`docs/tui.md`, "Themes").
//!
//! With no theme set, the theme follows the terminal's appearance and
//! switches when the terminal reports a change. The latest report wins;
//! before any report the appearance is dark.

use crate::keys::{Event, Reply};
use crate::look::Appearance;
use crate::theme::Rgb;

/// The appearance queries: OSC 11's background query, then the
/// theme-report enable (`CSI ? 2031 h`) and query (`CSI ? 996 n`).
pub(crate) const QUERIES: &[u8] = b"\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n";

/// What an OSC 11 candidate is: `buf` starts with `ESC ]`.
pub(crate) enum Osc {
    /// Every byte so far fits the grammar; the rest may follow.
    Hold,
    /// A complete reply: its events and the reply's length.
    Done(Vec<Event>, usize),
    /// The first byte off the grammar: read `ESC ]` as keys instead.
    Off,
}

/// Reads an OSC 11 background reply byte by byte: `ESC ] 1 1 ; r g b :`
/// three fields of 1 to 4 hex digits joined by `/`, then BEL or `ESC \`.
/// A buffer that ends while every byte so far fits is held. The first
/// byte that cannot continue the grammar ends the candidate, so a typed
/// Alt+] followed by text stays keys.
pub(crate) fn osc11(buf: &[u8]) -> Osc {
    let mut at = 2;
    for want in b"11;rgb:" {
        match buf.get(at) {
            None => return Osc::Hold,
            Some(byte) if byte == want => at = at.saturating_add(1),
            Some(_) => return Osc::Off,
        }
    }
    let mut rgb = (0u8, 0u8, 0u8);
    for field_at in 0..3 {
        let (value, next) = match field(buf, at) {
            Field::Hold => return Osc::Hold,
            Field::Off => return Osc::Off,
            Field::Value(value, next) => (value, next),
        };
        if field_at == 0 {
            rgb.0 = value;
        } else if field_at == 1 {
            rgb.1 = value;
        } else {
            rgb.2 = value;
        }
        at = next;
        if field_at < 2 {
            match buf.get(at) {
                None => return Osc::Hold,
                Some(b'/') => at = at.saturating_add(1),
                Some(_) => return Osc::Off,
            }
        }
    }
    let used = match buf.get(at) {
        None => return Osc::Hold,
        Some(0x07) => at.saturating_add(1),
        Some(0x1b) => match buf.get(at.saturating_add(1)) {
            None => return Osc::Hold,
            Some(b'\\') => at.saturating_add(2),
            Some(_) => return Osc::Off,
        },
        Some(_) => return Osc::Off,
    };
    let appearance = if dark(rgb) {
        Appearance::Dark
    } else {
        Appearance::Light
    };
    Osc::Done(vec![Event::Reply(Reply::Appearance(appearance))], used)
}

/// What one colour field is.
enum Field {
    /// The buffer ends mid-field; more digits may follow.
    Hold,
    /// No digits, or a fifth digit: the grammar ends here.
    Off,
    /// The field's 8-bit value and the byte after it.
    Value(u8, usize),
}

/// Reads one colour field at `at`: 1 to 4 hex digits.
fn field(buf: &[u8], mut at: usize) -> Field {
    let start = at;
    while matches!(buf.get(at), Some(byte) if byte.is_ascii_hexdigit()) {
        at = at.saturating_add(1);
        if at.saturating_sub(start) > 4 {
            return Field::Off;
        }
    }
    if buf.get(at).is_none() {
        return Field::Hold;
    }
    match scale(buf.get(start..at).unwrap_or_default()) {
        Some(value) => Field::Value(value, at),
        None => Field::Off,
    }
}

/// `digits` (1 to 4 hex digits) scaled to 8 bits; `None` for any other
/// length or a byte that is no hex digit.
fn scale(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let mut value = 0u32;
    for byte in digits {
        value = value.saturating_mul(16).saturating_add(hex(*byte)?);
    }
    let max = 16u32
        .pow(u32::try_from(digits.len()).unwrap_or(4))
        .saturating_sub(1);
    u8::try_from(value.saturating_mul(255) / max).ok()
}

/// The hex digit's value; `None` for any other byte.
fn hex(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some(u32::from(byte.saturating_sub(b'0'))),
        b'a'..=b'f' => Some(u32::from(byte.saturating_sub(b'a')).saturating_add(10)),
        b'A'..=b'F' => Some(u32::from(byte.saturating_sub(b'A')).saturating_add(10)),
        _ => None,
    }
}

/// Whether `rgb` is dark: its luma below mid grey.
fn dark(rgb: Rgb) -> bool {
    u32::from(rgb.0).saturating_mul(2126)
        + u32::from(rgb.1).saturating_mul(7152)
        + u32::from(rgb.2).saturating_mul(722)
        < 1_280_000
}

/// Reads a theme-report reply: `CSI ? 997 ; 1 n` is dark, `; 2` light;
/// anything else is nothing.
pub(crate) fn scheme(params: &[u8]) -> Vec<Event> {
    let appearance = match params {
        b"?997;1" => Appearance::Dark,
        b"?997;2" => Appearance::Light,
        _ => return Vec::new(),
    };
    vec![Event::Reply(Reply::Appearance(appearance))]
}

#[cfg(test)]
#[path = "appearance_tests.rs"]
mod tests;
