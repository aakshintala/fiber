//! The byte parser: terminal bytes to strokes, mouse reports and detection
//! replies (`docs/tui.md`, "Keys", "Mouse and hover"). [`default_event`]
//! maps a stroke to the key or edit the app's handlers match.

use crate::appearance::{self, Osc};
use crate::look::Appearance;
use crate::stroke::{Code, Mods, Stroke, fold_shift};

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
    /// Ctrl+G (`0x07`, `CSI 103;5u`): `open_in_editor`.
    CtrlG,
    /// Ctrl+R (`0x12`, `CSI 114;5u`): `search_prompts`.
    CtrlR,
    /// Ctrl+F (`0x06`, `CSI 102;5u`) and Cmd+F (`CSI 102;9u`):
    /// conversation search (`docs/tui.md`, "Search").
    CtrlF,
    /// Alt+Up (`CSI 1;3A`, or `ESC` then `CSI A` in one read):
    /// `select_steering`.
    AltUp,
    /// Alt+Down (`CSI 1;3B`, or `ESC` then `CSI B` in one read):
    /// `select_steering`.
    AltDown,
    /// Alt+X (`ESC x` in one read, `CSI 120;3u`): `drop_steering`.
    AltX,
    /// Alt+P (`ESC p` in one read, `CSI 112;3u`): `toggle_panel`.
    AltP,
    /// Alt+R (`ESC r` in one read, `CSI 114;3u`): `toggle_rail`.
    AltR,
    /// Alt+1 to Alt+9 (`ESC 1` to `ESC 9` in one read, `CSI 49;3u` to
    /// `CSI 57;3u`): `rail_row_n`, the digit.
    AltDigit(u8),
    /// Ctrl+V (`0x16`, `CSI 118;5u`): `paste_image`.
    CtrlV,
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
    /// A theme-report reply: the terminal's light or dark appearance.
    Appearance(Appearance),
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
    /// A key the terminal delivered. The parser emits only this, a pasted
    /// [`Edit::Paste`], [`Mouse`] and [`Reply`]; [`default_event`] maps one
    /// to the key or edit the app matches.
    Stroke(Stroke),
    /// A key from [`default_event`].
    Key(Key),
    /// A key that edits the draft.
    Edit(Edit),
    /// A mouse report.
    Mouse(Mouse),
    /// A detection reply.
    Reply(Reply),
}

/// The key or edit a stroke meant before rebinding: the parser's old mapping
/// from a delivered key to the [`Key`] or [`Edit`] the app's handlers match
/// (`docs/tui.md`, "Keys"). `None` for a stroke no binding uses.
pub(crate) fn default_event(stroke: &Stroke) -> Option<Event> {
    // Typing: a character alone is itself, and a shifted letter is its
    // uppercase form.
    if let Code::Char(ch) = stroke.code {
        if stroke.mods == Mods::NONE {
            return Some(Event::Key(Key::Char(ch)));
        }
        if stroke.mods == Mods::SHIFT {
            let mut upper = ch.to_uppercase();
            if let (Some(one), None) = (upper.next(), upper.next()) {
                return Some(Event::Key(Key::Char(one)));
            }
        }
        if stroke.mods == Mods::ALT
            && ('1'..='9').contains(&ch)
            && let Ok(n) = u8::try_from(ch)
        {
            return Some(Event::Key(Key::AltDigit(n.saturating_sub(b'0'))));
        }
    }
    let key = |key: Key| Some(Event::Key(key));
    let edit = |edit: Edit| Some(Event::Edit(edit));
    match stroke.name().as_str() {
        "enter" => key(Key::Enter),
        "shift+enter" => edit(Edit::ShiftEnter),
        "esc" => key(Key::Esc),
        "backspace" => key(Key::Backspace),
        "alt+backspace" => edit(Edit::DeleteWord),
        "tab" => key(Key::Tab),
        "shift+tab" => key(Key::BackTab),
        "space" => key(Key::Char(' ')),
        "pageup" => key(Key::PageUp),
        "pagedown" => key(Key::PageDown),
        "end" => key(Key::End),
        "up" => key(Key::Up),
        "alt+up" => key(Key::AltUp),
        "down" => key(Key::Down),
        "alt+down" => key(Key::AltDown),
        "left" => edit(Edit::Left),
        "right" => edit(Edit::Right),
        "alt+left" | "ctrl+left" | "alt+b" => edit(Edit::WordLeft),
        "alt+right" | "ctrl+right" | "alt+f" => edit(Edit::WordRight),
        "super+left" => edit(Edit::LineStart),
        "super+right" => edit(Edit::LineEnd),
        "delete" => edit(Edit::Delete),
        "f1" => key(Key::F1),
        "ctrl+c" => key(Key::CtrlC),
        "ctrl+o" => key(Key::CtrlO),
        "ctrl+g" => key(Key::CtrlG),
        "ctrl+r" => key(Key::CtrlR),
        "ctrl+v" => key(Key::CtrlV),
        "ctrl+f" | "super+f" => key(Key::CtrlF),
        "ctrl+j" => edit(Edit::CtrlJ),
        "alt+a" => key(Key::AltA),
        "alt+x" => key(Key::AltX),
        "alt+p" => key(Key::AltP),
        "alt+r" => key(Key::AltR),
        _ => None,
    }
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
/// Every modifier bit this file reads: shift 1, alt 2, ctrl 4, super 8.
/// A literal, not `SHIFT | ALT | CTRL | SUPER`, so no operator is left for
/// a mutation to swap; keep it in step with the four constants above.
const KNOWN_MODS: u32 = 0b1111;
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

    /// Whether kitty's flags are pushed.
    pub(crate) fn kitty(&self) -> bool {
        self.kitty
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
    let stroke = |code: Code, mods: Mods| Some((vec![Event::Stroke(Stroke { code, mods })], 1));
    let byte = *buf.first()?;
    if let Some((code, mods)) = control_stroke(byte) {
        return stroke(code, mods);
    }
    match byte {
        // A lone ESC ending the read is Esc; ESC followed by bytes in the
        // same read starts a sequence.
        0x1b => match buf.get(1) {
            None => stroke(Code::Esc, Mods::NONE),
            Some(b'[') => parse_csi(buf),
            Some(b'O') => parse_ss3(buf),
            // An OSC reply starts here; anything off its grammar stays
            // keys, so a typed Alt+] never swallows text.
            Some(b']') => match appearance::osc11(buf) {
                Osc::Hold => None,
                Osc::Done(events, used) => Some((events, used)),
                Osc::Off => alt_stroke(buf),
            },
            // ESC before an arrow's CSI is the legacy Alt arrow.
            Some(0x1b) if buf.get(2) == Some(&b'[') => {
                let (events, used) = parse_csi(buf.get(1..)?)?;
                let alt = events
                    .into_iter()
                    .filter_map(|event| {
                        if let Event::Stroke(pressed) = event
                            && pressed.mods == Mods::NONE
                            && (pressed.code == Code::Up || pressed.code == Code::Down)
                        {
                            Some(Event::Stroke(Stroke {
                                code: pressed.code,
                                mods: Mods::ALT,
                            }))
                        } else {
                            None
                        }
                    })
                    .collect();
                Some((alt, used.saturating_add(1)))
            }
            Some(_) => alt_stroke(buf),
        },
        _ => decode_char(buf),
    }
}

/// The stroke a lone control byte names: `0x00` is Ctrl+Space, `0x01` to
/// `0x1a` Ctrl with A to Z (Backspace, Tab, Ctrl+J and Enter keep their own
/// strokes), `0x1c` to `0x1f` Ctrl with `\`, `]`, `^` and `_`, and `0x7f`
/// Backspace. `None` for any other byte.
fn control_stroke(byte: u8) -> Option<(Code, Mods)> {
    let ctrl = Mods::CTRL;
    match byte {
        0x00 => Some((Code::Space, ctrl)),
        0x01..=0x07 | 0x0b..=0x0c | 0x0e..=0x1a => {
            Some((Code::Char((byte - 1 + b'a') as char), ctrl))
        }
        0x08 | 0x7f => Some((Code::Backspace, Mods::NONE)),
        0x09 => Some((Code::Tab, Mods::NONE)),
        0x0a => Some((Code::Char('j'), ctrl)),
        0x0d => Some((Code::Enter, Mods::NONE)),
        0x1c => Some((Code::Char('\\'), ctrl)),
        0x1d => Some((Code::Char(']'), ctrl)),
        0x1e => Some((Code::Char('^'), ctrl)),
        0x1f => Some((Code::Char('_'), ctrl)),
        _ => None,
    }
}

/// `ESC` with the byte after it in the same read: Alt with that key's
/// stroke (`ESC 0x7f` is Alt+Backspace, `ESC` with a control byte Ctrl+Alt
/// with that letter). `0x08` is Ctrl+H, so `ESC 0x08` is Ctrl+Alt+H, which
/// no binding uses; the old parser dropped it.
fn alt_stroke(buf: &[u8]) -> Step {
    let tail = buf.get(1..)?;
    if tail.first() == Some(&0x08) {
        let unbound = Event::Stroke(Stroke {
            code: Code::Char('h'),
            mods: Mods::CTRL | Mods::ALT,
        });
        return Some((vec![unbound], 2));
    }
    if let Some(byte) = tail.first()
        && let Some((code, mods)) = control_stroke(*byte)
    {
        let alt = Event::Stroke(Stroke {
            code,
            mods: mods | Mods::ALT,
        });
        return Some((vec![alt], 2));
    }
    let (mut events, used) = decode_char(tail)?;
    for event in &mut events {
        if let Event::Stroke(pressed) = event {
            pressed.mods = pressed.mods | Mods::ALT;
        }
    }
    Some((events, used.saturating_add(1)))
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
        // The theme report: `CSI ? 997 ; 1 n` dark, `; 2` light. `scheme`
        // takes only those two, so anything else is nothing.
        0x6e => appearance::scheme(params),
        0x75 => kitty_key(params).into_iter().collect(),
        0x7e => tilde_key(params).into_iter().collect(),
        b'M' | b'm' if params.first() == Some(&b'<') => {
            let params = params.get(1..).unwrap_or_default();
            sgr_mouse(params, final_byte == b'm')
                .map_or_else(Vec::new, |mouse| vec![Event::Mouse(mouse)])
        }
        0x41 | 0x42 | 0x43 | 0x44 | 0x48 | 0x46 => {
            csi_letter(final_byte, params).into_iter().collect()
        }
        0x5a if params.is_empty() => vec![Event::Stroke(Stroke {
            code: Code::Tab,
            mods: Mods::SHIFT,
        })],
        0x50 if params.is_empty() => vec![Event::Stroke(Stroke {
            code: Code::F(1),
            mods: Mods::NONE,
        })],
        _ => Vec::new(),
    };
    Some((events, end.saturating_add(1)))
}

/// The SGR mouse report with parameters `Cb;Cx;Cy` (after the `<`), its
/// final byte `m` when `release`. `None` for a malformed report: a
/// parameter that is not all digits, not three parameters, a coordinate
/// of 0 or above `u16::MAX`, a press of no button, a horizontal wheel or
/// a button above 7. A report with Shift held is `None` too.
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
    // Shift is bit 4: a terminal that reports a Shift-drag leaves it the
    // terminal's native selection (`docs/tui.md`, "Selection and copy"), so
    // a Shift report is no click, selection or hover. Alt and Ctrl are bits
    // 8 and 16 (0b1_1000); they are ignored.
    if cb & 4 != 0 {
        return None;
    }
    let cb = cb & !0b1_1000;
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

/// `CSI` with an arrow, Home or End final byte (`A`, `B`, `C`, `D`, `H`,
/// `F`): that key, plain or `CSI 1;<modifiers>` with the kitty modifier
/// bits. A bare `1` is no modifiers for `C` and `D` only, as the old
/// `arrow` read it; a bare `1` with any other letter is nothing, as the
/// old arms read it. A modifier the bindings do not name (hyper, meta)
/// reads as nothing.
fn csi_letter(final_byte: u8, params: &[u8]) -> Option<Event> {
    let mods = if params.is_empty() {
        Mods::NONE
    } else {
        let text = std::str::from_utf8(params).ok()?;
        match text.split_once(';') {
            Some((one, rest)) => {
                if one.split(':').next() != Some("1") {
                    return None;
                }
                modifiers(rest)?
            }
            // A bare `1` is no modifiers for `C` and `D`, as the old
            // `arrow` read it; any other bare parameter is nothing.
            None => {
                if text.split(':').next() != Some("1") {
                    return None;
                }
                if !matches!(final_byte, 0x43 | 0x44) {
                    return None;
                }
                Mods::NONE
            }
        }
    };
    let code = match final_byte {
        0x41 => Code::Up,
        0x42 => Code::Down,
        0x43 => Code::Right,
        0x44 => Code::Left,
        0x48 => Code::Home,
        0x46 => Code::End,
        _ => return None,
    };
    Some(Event::Stroke(Stroke { code, mods }))
}

/// `CSI n[;<modifiers>] ~`: Insert, Delete, Home, End, PageUp, PageDown and
/// F1 to F12, plain or with the kitty modifier bits.
fn tilde_key(params: &[u8]) -> Option<Event> {
    let text = std::str::from_utf8(params).ok()?;
    let (num, mods) = match text.split_once(';') {
        Some((num, rest)) => (num, modifiers(rest)?),
        None => (text, Mods::NONE),
    };
    let code = match num {
        "2" => Code::Insert,
        "3" => Code::Delete,
        "5" => Code::PageUp,
        "6" => Code::PageDown,
        "1" | "7" => Code::Home,
        "4" | "8" => Code::End,
        "11" => Code::F(1),
        "12" => Code::F(2),
        "13" => Code::F(3),
        "14" => Code::F(4),
        "15" => Code::F(5),
        "17" => Code::F(6),
        "18" => Code::F(7),
        "19" => Code::F(8),
        "20" => Code::F(9),
        "21" => Code::F(10),
        "23" => Code::F(11),
        "24" => Code::F(12),
        _ => return None,
    };
    Some(Event::Stroke(Stroke { code, mods }))
}

/// The kitty modifier bits in a `;<modifiers>` field. Locks change no
/// binding; hyper and meta have no stroke, so a field naming one reads as
/// nothing.
fn modifiers(field: &str) -> Option<Mods> {
    let bits = number(field.split(':').next()?)?.saturating_sub(1) & !LOCKS;
    if bits & !KNOWN_MODS != 0 {
        return None;
    }
    Some(mods_from(bits))
}

/// The modifiers the kitty bits name.
fn mods_from(bits: u32) -> Mods {
    let mut mods = Mods::NONE;
    if bits & SHIFT != 0 {
        mods = mods | Mods::SHIFT;
    }
    if bits & ALT != 0 {
        mods = mods | Mods::ALT;
    }
    if bits & CTRL != 0 {
        mods = mods | Mods::CTRL;
    }
    if bits & SUPER != 0 {
        mods = mods | Mods::SUPER;
    }
    mods
}

/// Parses `ESC O` at the start of `buf`.
fn parse_ss3(buf: &[u8]) -> Step {
    let stroke = |code: Code| {
        Some((
            vec![Event::Stroke(Stroke {
                code,
                mods: Mods::NONE,
            })],
            3,
        ))
    };
    match *buf.get(2)? {
        b'F' => stroke(Code::End),
        b'A' => stroke(Code::Up),
        b'B' => stroke(Code::Down),
        b'C' => stroke(Code::Right),
        b'D' => stroke(Code::Left),
        b'H' => stroke(Code::Home),
        b'P' => stroke(Code::F(1)),
        b'Q' => stroke(Code::F(2)),
        b'R' => stroke(Code::F(3)),
        b'S' => stroke(Code::F(4)),
        _ => Some((Vec::new(), 3)),
    }
}

/// A decimal field: digits only, as `u32`'s parser would also take a
/// leading `+`. An empty field is none through the parse below.
fn number(field: &str) -> Option<u32> {
    if !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// A kitty key, `CSI <code>[;<modifiers>] u`: Enter, Esc, Tab, Backspace,
/// the space bar and any other character, with the kitty modifier bits.
/// Locks change no binding, and a sub-field after a colon changes nothing;
/// hyper and meta have no stroke, and private-use codes read as nothing.
fn kitty_key(params: &[u8]) -> Option<Event> {
    let text = std::str::from_utf8(params).ok()?;
    let mut fields = text.split(';');
    let code: u32 = number(fields.next()?.split(':').next()?)?;
    let mut bits = match fields.next() {
        None => 1,
        Some(field) => number(field.split(':').next()?)?,
    }
    .saturating_sub(1)
        & !LOCKS;
    if code >= 57344 {
        return None;
    }
    if bits & !KNOWN_MODS != 0 {
        return None;
    }
    let key = match code {
        13 => Code::Enter,
        27 => Code::Esc,
        9 => Code::Tab,
        127 => Code::Backspace,
        _ => {
            let ch = char::from_u32(code)?;
            if ch.is_control() {
                return None;
            }
            // Shift on a character that is not a letter is dropped.
            if ch != ' ' && !ch.is_alphabetic() {
                bits &= !SHIFT;
            }
            let (code, mods) = char_stroke(ch, mods_from(bits));
            return Some(Event::Stroke(Stroke { code, mods }));
        }
    };
    Some(Event::Stroke(Stroke {
        code: key,
        mods: mods_from(bits),
    }))
}

/// The stroke for character `ch` delivered with `mods`: the space bar is
/// `Space`, and an uppercase letter arrives with shift held, but only when
/// shift round-trips back to the letter (`ß` stays `ß`, `ẞ` stays `ẞ`).
/// Shift on a character that is not a letter is the caller's to drop.
fn char_stroke(ch: char, mods: Mods) -> (Code, Mods) {
    if ch == ' ' {
        return (Code::Space, mods);
    }
    let (base, shifted) = fold_shift(ch);
    if shifted {
        (Code::Char(base), mods | Mods::SHIFT)
    } else {
        (Code::Char(base), mods)
    }
}

/// Decodes one character at the start of `buf`: an uppercase letter arrives
/// with shift held, and the space bar is `Space`. A control character is
/// dropped whole; a byte that starts no character is dropped alone.
fn decode_char(buf: &[u8]) -> Step {
    let stroke = |code: Code, mods: Mods, len: usize| {
        Some((vec![Event::Stroke(Stroke { code, mods })], len))
    };
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
        Some(ch) => {
            let (code, mods) = char_stroke(ch, Mods::NONE);
            stroke(code, mods, len)
        }
        None => Some((Vec::new(), 1)),
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
