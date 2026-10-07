//! The byte parser: terminal bytes to keys, mouse reports and detection
//! replies (`docs/tui.md`, "Keys", "Mouse and hover").

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

/// One key that edits the draft (`docs/tui.md`, "The input box",
/// "Bindings"). Kept apart from [`Key`]: the approval panel reads only
/// [`Key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Edit {
    /// Left (`CSI D`, `SS3 D`).
    Left,
    /// Right (`CSI C`, `SS3 C`).
    Right,
    /// Shift+Enter (`CSI 13;2u`).
    ShiftEnter,
    /// Ctrl+J (`0x0a`, `CSI 106;5u`).
    CtrlJ,
    /// ⌥← or Ctrl+← (`CSI 1;3D`, `CSI 1;5D`, `ESC b`, `CSI 98;3u`).
    WordLeft,
    /// ⌥→ or Ctrl+→ (`CSI 1;3C`, `CSI 1;5C`, `ESC f`, `CSI 102;3u`).
    WordRight,
    /// ⌥Backspace (`ESC 0x7f`, `CSI 127;3u`).
    DeleteWord,
    /// ⌘← (`CSI 1;9D`).
    LineStart,
    /// ⌘→ (`CSI 1;9C`).
    LineEnd,
    /// Delete (`CSI 3~`).
    Delete,
    /// A bracketed paste's text: line breaks as `\n`, and no control
    /// character but `\n` and `\t`.
    Paste(String),
}

/// One detection reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reply {
    /// Kitty keyboard flags (`CSI ? <flags> u`).
    KittyFlags(u8),
    /// Primary device attributes (`CSI ? ... c`).
    DeviceAttributes,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    Left,
    Middle,
    Right,
}

/// What a mouse report says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseKind {
    /// A button went down.
    Press(Button),
    /// A button went up; SGR may not say which.
    Release,
    /// The pointer moved with no button held.
    Motion,
    /// The pointer moved with a button held.
    Drag(Button),
    /// The wheel turned up.
    WheelUp,
    /// The wheel turned down.
    WheelDown,
}

/// One SGR mouse report (`CSI < Cb ; Cx ; Cy M` or `m`), at a 0-based
/// cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mouse {
    pub(crate) kind: MouseKind,
    pub(crate) col: u16,
    pub(crate) row: u16,
}

/// One parsed event: a key, a mouse report or a detection reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// A key.
    Key(Key),
    /// A key that edits the draft.
    Edit(Edit),
    /// A mouse report.
    Mouse(Mouse),
    /// A detection reply.
    Reply(Reply),
}

/// Starts a bracketed paste.
const PASTE_START: &[u8] = b"\x1b[200~";
/// Ends a bracketed paste.
const PASTE_END: &[u8] = b"\x1b[201~";

/// Kitty's modifier bits, after the 1 its field adds
/// (`docs/tui.md`, "Keys").
const SHIFT: u32 = 1;
/// Alt, or ⌥.
const ALT: u32 = 2;
/// Ctrl.
const CTRL: u32 = 4;
/// Super, or ⌘.
const SUPER: u32 = 8;
/// Caps Lock (64) and Num Lock (128), which change no binding.
const LOCKS: u32 = 0b1100_0000;

/// Parses terminal bytes. An incomplete CSI or UTF-8 sequence at the end of
/// a read is held for the next read; a lone `0x1b` ending a read is Esc,
/// until kitty's flags are pushed. A bracketed paste is held until its end
/// marker, however many reads it takes, and yields no key.
#[derive(Debug, Default)]
pub(crate) struct Parser {
    /// Unprocessed tail from the previous read.
    pending: Vec<u8>,
    /// The bytes of a bracketed paste whose end has not arrived.
    paste: Option<Vec<u8>>,
    /// Whether kitty's flags are pushed: Esc is then `CSI 27u`, so a lone
    /// `0x1b` ending a read always starts a sequence and is held.
    kitty: bool,
}

impl Parser {
    /// Records that kitty's flags are pushed.
    pub(crate) fn set_kitty(&mut self) {
        self.kitty = true;
    }

    /// Feeds one read's bytes, returning its events in order.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut rest = buf.as_slice();
        loop {
            if let Some(paste) = &mut self.paste {
                // The end marker may have started in an earlier read: look
                // again from where a split one would begin.
                let held = paste.len();
                let from = held.saturating_sub(PASTE_END.len().saturating_sub(1));
                paste.extend_from_slice(rest);
                let Some(at) = find(paste, PASTE_END, from) else {
                    return out;
                };
                // The marker ends in this read, so its end is past `held`.
                let after = at.saturating_add(PASTE_END.len()).saturating_sub(held);
                paste.truncate(at);
                let text = paste_text(paste);
                self.paste = None;
                if !text.is_empty() {
                    out.push(Event::Edit(Edit::Paste(text)));
                }
                rest = rest.get(after..).unwrap_or_default();
                continue;
            }
            if let Some(after) = rest.strip_prefix(PASTE_START) {
                self.paste = Some(Vec::new());
                rest = after;
                continue;
            }
            // debt: without kitty's flags a lone ESC ending a read is Esc, so
            // a paste start marker split right after its ESC reads as Esc and
            // the paste as keys. Ceiling: only a read boundary landing exactly
            // after that ESC. Upgrade trigger: a report of a paste typed as
            // keys on a terminal without kitty's keyboard protocol; the fix
            // is an Esc timeout.
            if self.kitty && rest == [0x1b] {
                break;
            }
            let Some((events, used)) = step(rest) else {
                break;
            };
            out.extend(events);
            // Every step takes at least one byte, so the loop ends.
            rest = rest.get(used.max(1)..).unwrap_or_default();
        }
        self.pending = rest.to_vec();
        out
    }
}

/// Where `needle` first starts in `haystack` at or after `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at.saturating_add(from))
}

/// A paste's text: `\r\n` and `\r` become `\n`, and every control
/// character but `\n` and `\t` goes.
fn paste_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
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
        0x0a => Some((vec![Event::Edit(Edit::CtrlJ)], 1)),
        0x09 => key(Key::Tab),
        0x0d => key(Key::Enter),
        // A lone ESC ending the read is Esc; ESC followed by bytes in the
        // same read starts a sequence.
        0x1b => match buf.get(1) {
            None => key(Key::Esc),
            Some(b'[') => parse_csi(buf),
            Some(b'O') => parse_ss3(buf),
            Some(b'a') => Some((vec![Event::Key(Key::AltA)], 2)),
            Some(b'b') => Some((vec![Event::Edit(Edit::WordLeft)], 2)),
            Some(b'f') => Some((vec![Event::Edit(Edit::WordRight)], 2)),
            Some(0x7f) => Some((vec![Event::Edit(Edit::DeleteWord)], 2)),
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
        0x75 => kitty_key(params).into_iter().collect(),
        0x7e => match params {
            [b'5'] => vec![Event::Key(Key::PageUp)],
            [b'6'] => vec![Event::Key(Key::PageDown)],
            [b'4'] => vec![Event::Key(Key::End)],
            [b'3'] => vec![Event::Edit(Edit::Delete)],
            [b'1', b'1'] => vec![Event::Key(Key::F1)],
            _ => Vec::new(),
        },
        b'M' | b'm' if params.first() == Some(&b'<') => {
            let params = params.get(1..).unwrap_or_default();
            sgr_mouse(params, final_byte == b'm').map_or_else(Vec::new, |mouse| {
                vec![Event::Mouse(mouse)]
            })
        }
        0x46 if params.is_empty() => vec![Event::Key(Key::End)],
        0x41 if params.is_empty() => vec![Event::Key(Key::Up)],
        0x42 if params.is_empty() => vec![Event::Key(Key::Down)],
        0x43 | 0x44 => arrow(final_byte == 0x43, params).into_iter().collect(),
        0x5a if params.is_empty() => vec![Event::Key(Key::BackTab)],
        0x50 if params.is_empty() => vec![Event::Key(Key::F1)],
        _ => Vec::new(),
    };
    Some((events, end.saturating_add(1)))
}

/// The SGR mouse report with parameters `Cb;Cx;Cy` (after the `<`), its
/// final byte `m` when `release`. `None` for a malformed report: a
/// parameter that is not all digits, not three parameters, a coordinate
/// of 0 or above `u16::MAX`, a press of no button, a horizontal wheel or
/// a button above 7.
fn sgr_mouse(params: &[u8], release: bool) -> Option<Mouse> {
    let mut fields = params.split(|byte| *byte == b';').map(|field| {
        if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(field).ok()?.parse::<u32>().ok()
    });
    let cb = fields.next()??;
    let col = u16::try_from(fields.next()??).ok()?.checked_sub(1)?;
    let row = u16::try_from(fields.next()??).ok()?.checked_sub(1)?;
    if fields.next().is_some() {
        return None;
    }
    // Shift, Alt and Ctrl are bits 4, 8 and 16; they are ignored.
    let cb = cb & !(4 | 8 | 16);
    let button = match cb & 3 {
        0 => Some(Button::Left),
        1 => Some(Button::Middle),
        2 => Some(Button::Right),
        _ => None,
    };
    let kind = if cb >= 128 {
        return None;
    } else if cb & 64 != 0 {
        match cb & 3 {
            0 => MouseKind::WheelUp,
            1 => MouseKind::WheelDown,
            _ => return None,
        }
    } else if cb & 32 != 0 {
        button.map_or(MouseKind::Motion, MouseKind::Drag)
    } else if release {
        MouseKind::Release
    } else {
        MouseKind::Press(button?)
    };
    Some(Mouse { kind, col, row })
}

/// Parses `ESC O` at the start of `buf`.
fn parse_ss3(buf: &[u8]) -> Step {
    let events = match *buf.get(2)? {
        b'F' => vec![Event::Key(Key::End)],
        b'A' => vec![Event::Key(Key::Up)],
        b'B' => vec![Event::Key(Key::Down)],
        b'C' => vec![Event::Edit(Edit::Right)],
        b'D' => vec![Event::Edit(Edit::Left)],
        b'P' => vec![Event::Key(Key::F1)],
        _ => Vec::new(),
    };
    Some((events, 3))
}

/// A decimal field: digits only, as `u32`'s parser would also take a
/// leading `+`.
fn number(field: &str) -> Option<u32> {
    if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// A `CSI` key's parameters, `<code>[:...][;<modifiers>[:...]]`: the code
/// and the modifier bits without the lock keys. `None` when they do not
/// parse.
fn code_and_mods(params: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(params).ok()?;
    let mut fields = text.split(';');
    let code = number(fields.next()?.split(':').next()?)?;
    let mods = match fields.next() {
        None => 1,
        Some(field) => number(field.split(':').next()?)?,
    };
    Some((code, mods.saturating_sub(1) & !LOCKS))
}

/// A kitty key, `CSI <code>[;<modifiers>] u`: the bound ones only.
fn kitty_key(params: &[u8]) -> Option<Event> {
    let (code, mods) = code_and_mods(params)?;
    let event = match (code, mods) {
        (13, 0) => Event::Key(Key::Enter),
        (13, SHIFT) => Event::Edit(Edit::ShiftEnter),
        (27, 0) => Event::Key(Key::Esc),
        (127, 0) => Event::Key(Key::Backspace),
        (127, ALT) => Event::Edit(Edit::DeleteWord),
        (99, CTRL) => Event::Key(Key::CtrlC),
        (111, CTRL) => Event::Key(Key::CtrlO),
        (106, CTRL) => Event::Edit(Edit::CtrlJ),
        (9, 0) => Event::Key(Key::Tab),
        (9, SHIFT) => Event::Key(Key::BackTab),
        (97, ALT) => Event::Key(Key::AltA),
        (98, ALT) => Event::Edit(Edit::WordLeft),
        (102, ALT) => Event::Edit(Edit::WordRight),
        _ => return None,
    };
    Some(event)
}

/// `CSI C` or `CSI D`, plain or `CSI 1;<modifiers>`: by character, word
/// (⌥ or Ctrl) or line (⌘).
fn arrow(right: bool, params: &[u8]) -> Option<Event> {
    let mods = if params.is_empty() {
        0
    } else {
        match code_and_mods(params)? {
            (1, mods) => mods,
            _ => return None,
        }
    };
    let edit = match (mods, right) {
        (0, true) => Edit::Right,
        (0, false) => Edit::Left,
        (ALT | CTRL, true) => Edit::WordRight,
        (ALT | CTRL, false) => Edit::WordLeft,
        (SUPER, true) => Edit::LineEnd,
        (SUPER, false) => Edit::LineStart,
        _ => return None,
    };
    Some(Event::Edit(edit))
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
