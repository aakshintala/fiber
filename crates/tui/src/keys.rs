//! The byte parser: terminal bytes to keys and detection replies
//! (`docs/tui.md`, "Keys").

/// One key this slice handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Key {
    /// A printable character.
    Char(char),
    /// Backspace (`0x7f` or `0x08`).
    Backspace,
    /// Enter (`\r`).
    Enter,
    /// Escape.
    Esc,
    /// Ctrl+C (`0x03`).
    CtrlC,
    /// Ctrl+O (`0x0f`): `toggle_ledgers`.
    CtrlO,
    /// PageUp (`CSI 5~`).
    PageUp,
    /// PageDown (`CSI 6~`).
    PageDown,
    /// End (`CSI F`, `CSI 4~`, `SS3 F`).
    End,
    /// Up (`CSI A`, `SS3 A`).
    Up,
    /// Down (`CSI B`, `SS3 B`).
    Down,
    /// Alt+A (`ESC a` in one read).
    AltA,
    /// Tab (`0x09`).
    Tab,
    /// Shift+Tab (`CSI Z`).
    BackTab,
    /// F1 (`SS3 P`, `CSI 11~`, `CSI P`).
    F1,
}

/// One detection reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reply {
    /// Kitty keyboard flags (`CSI ? <flags> u`).
    KittyFlags(u8),
    /// Primary device attributes (`CSI ? ... c`).
    DeviceAttributes,
}

/// One parsed event: a key or a detection reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// A key.
    Key(Key),
    /// A detection reply.
    Reply(Reply),
}

/// Parses terminal bytes. An incomplete CSI or UTF-8 sequence at the end of
/// a read is held for the next read; a lone `0x1b` ending a read is Esc.
#[derive(Debug, Default)]
pub(crate) struct Parser {
    /// Unprocessed tail from the previous read.
    pending: Vec<u8>,
}

impl Parser {
    /// Feeds one read's bytes, returning its events in order.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut rest = buf.as_slice();
        while let Some((events, used)) = step(rest) {
            out.extend(events);
            // Every step takes at least one byte, so the loop ends.
            rest = rest.get(used.max(1)..).unwrap_or_default();
        }
        self.pending = rest.to_vec();
        out
    }
}

/// The events of the bytes at the start of a buffer and how many bytes
/// they take; `None` when the buffer is empty, or starts with an
/// incomplete sequence held for the next read.
type Step = Option<(Vec<Event>, usize)>;

/// Parses the bytes at the start of `buf`.
fn step(buf: &[u8]) -> Step {
    let key = |key: Key| Some((vec![Event::Key(key)], 1));
    match *buf.first()? {
        0x03 => key(Key::CtrlC),
        0x0f => key(Key::CtrlO),
        0x08 | 0x7f => key(Key::Backspace),
        0x09 => key(Key::Tab),
        0x0d => key(Key::Enter),
        // A lone ESC ending the read is Esc; ESC followed by bytes in the
        // same read starts a sequence.
        0x1b => match buf.get(1) {
            None => key(Key::Esc),
            Some(b'[') => parse_csi(buf),
            Some(b'O') => parse_ss3(buf),
            Some(b'a') => Some((vec![Event::Key(Key::AltA)], 2)),
            // Unknown escape sequence: drop ESC and the byte after it.
            Some(_) => Some((Vec::new(), 2)),
        },
        _ => decode_char(buf),
    }
}

/// Parses `ESC [` at the start of `buf`.
fn parse_csi(buf: &[u8]) -> Step {
    // Find the final byte: 0x40..=0x7e. Parameters are 0x30..=0x3f,
    // intermediates 0x20..=0x2f.
    let mut end = None;
    for (at, byte) in buf.iter().copied().enumerate().skip(2) {
        if (0x40..=0x7e).contains(&byte) {
            end = Some(at);
            break;
        }
        if !(0x20..=0x3f).contains(&byte) {
            // Not a CSI byte at all: drop `ESC [` and reparse after it.
            return Some((Vec::new(), 2));
        }
    }
    let end = end?;
    let final_byte = *buf.get(end)?;
    let params = buf.get(2..end)?;
    let events = match final_byte {
        0x75 if params.first() == Some(&b'?') => {
            // Kitty flags: `CSI ? <flags> u`. Digits only: `u8`'s parser
            // would also take a leading `+`.
            let digits = params.get(1..).unwrap_or_default();
            if digits.iter().all(u8::is_ascii_digit) {
                String::from_utf8_lossy(digits).parse::<u8>().map_or_else(
                    |_| Vec::new(),
                    |flags| vec![Event::Reply(Reply::KittyFlags(flags))],
                )
            } else {
                Vec::new()
            }
        }
        0x63 if params.first() == Some(&b'?') => {
            // DA1: `CSI ? ... c` ends detection.
            vec![Event::Reply(Reply::DeviceAttributes)]
        }
        0x7e => match params {
            [b'5'] => vec![Event::Key(Key::PageUp)],
            [b'6'] => vec![Event::Key(Key::PageDown)],
            [b'4'] => vec![Event::Key(Key::End)],
            [b'1', b'1'] => vec![Event::Key(Key::F1)],
            _ => Vec::new(),
        },
        0x46 if params.is_empty() => vec![Event::Key(Key::End)],
        0x41 if params.is_empty() => vec![Event::Key(Key::Up)],
        0x42 if params.is_empty() => vec![Event::Key(Key::Down)],
        0x5a if params.is_empty() => vec![Event::Key(Key::BackTab)],
        0x50 if params.is_empty() => vec![Event::Key(Key::F1)],
        _ => Vec::new(),
    };
    Some((events, end.saturating_add(1)))
}

/// Parses `ESC O` at the start of `buf`.
fn parse_ss3(buf: &[u8]) -> Step {
    let events = match *buf.get(2)? {
        b'F' => vec![Event::Key(Key::End)],
        b'A' => vec![Event::Key(Key::Up)],
        b'B' => vec![Event::Key(Key::Down)],
        b'P' => vec![Event::Key(Key::F1)],
        _ => Vec::new(),
    };
    Some((events, 3))
}

/// Decodes one character at the start of `buf`. A control character is
/// dropped whole; a byte that starts no character is dropped alone.
fn decode_char(buf: &[u8]) -> Step {
    let len = match *buf.first()? {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Some((Vec::new(), 1)),
    };
    let Some(head) = buf.get(..len) else {
        // Truncated only when every byte so far continues correctly;
        // otherwise the lead byte is invalid.
        let continues = buf.iter().skip(1).all(|b| b & 0xc0 == 0x80);
        return if continues {
            None
        } else {
            Some((Vec::new(), 1))
        };
    };
    match std::str::from_utf8(head)
        .ok()
        .and_then(|text| text.chars().next())
    {
        Some(ch) if ch.is_control() => Some((Vec::new(), len)),
        Some(ch) => Some((vec![Event::Key(Key::Char(ch))], len)),
        None => Some((Vec::new(), 1)),
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
