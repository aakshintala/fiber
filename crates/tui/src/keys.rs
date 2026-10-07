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
    /// PageUp (`CSI 5~`).
    PageUp,
    /// PageDown (`CSI 6~`).
    PageDown,
    /// End (`CSI F`, `CSI 4~`, `SS3 F`).
    End,
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
        let mut i = 0;
        while i < buf.len() {
            let Some(byte) = buf.get(i).copied() else {
                break;
            };
            match byte {
                0x03 => {
                    out.push(Event::Key(Key::CtrlC));
                    i += 1;
                }
                0x08 | 0x7f => {
                    out.push(Event::Key(Key::Backspace));
                    i += 1;
                }
                0x0d => {
                    out.push(Event::Key(Key::Enter));
                    i += 1;
                }
                0x1b => {
                    // A lone ESC ending the read is Esc; ESC followed by
                    // bytes in the same read starts a sequence.
                    let Some(next) = buf.get(i.saturating_add(1)).copied() else {
                        out.push(Event::Key(Key::Esc));
                        i += 1;
                        continue;
                    };
                    match next {
                        0x5b => {
                            let Some(rest) = buf.get(i..) else {
                                break;
                            };
                            match parse_csi(rest) {
                                Csi::Complete(events, len) => {
                                    out.extend(events);
                                    i += len;
                                }
                                Csi::Incomplete => break,
                            }
                        }
                        0x4f => {
                            let Some(rest) = buf.get(i..) else {
                                break;
                            };
                            match parse_ss3(rest) {
                                Ss3::Complete(events, len) => {
                                    out.extend(events);
                                    i += len;
                                }
                                Ss3::Incomplete => break,
                            }
                        }
                        _ => {
                            // Unknown escape sequence: drop ESC and the
                            // byte after it.
                            i += 2;
                        }
                    }
                }
                _ => {
                    let Some(rest) = buf.get(i..) else {
                        break;
                    };
                    match decode_char(rest) {
                        Char::Done(ch, len) => {
                            if !ch.is_control() {
                                out.push(Event::Key(Key::Char(ch)));
                            }
                            i += len;
                        }
                        Char::Incomplete => break,
                        Char::Invalid => {
                            i += 1;
                        }
                    }
                }
            }
        }
        self.pending = buf.get(i..).map_or_else(Vec::new, <[u8]>::to_vec);
        out
    }
}

/// One CSI parse result.
enum Csi {
    /// Parsed events and bytes consumed.
    Complete(Vec<Event>, usize),
    /// No final byte yet; held for the next read.
    Incomplete,
}

/// Parses `ESC [` at the start of `buf`.
fn parse_csi(buf: &[u8]) -> Csi {
    // Find the final byte: 0x40..=0x7e. Parameters are 0x30..=0x3f,
    // intermediates 0x20..=0x2f.
    let mut end = None;
    for at in 2..buf.len().saturating_add(1) {
        let Some(byte) = buf.get(at).copied() else {
            break;
        };
        if (0x40..=0x7e).contains(&byte) {
            end = Some(at);
            break;
        }
        if !(0x20..=0x3f).contains(&byte) {
            // Not a CSI byte at all: drop `ESC [` and reparse after it.
            return Csi::Complete(Vec::new(), 2);
        }
    }
    let Some(end) = end else {
        return Csi::Incomplete;
    };
    let Some(final_byte) = buf.get(end).copied() else {
        return Csi::Incomplete;
    };
    let Some(params) = buf.get(2..end) else {
        return Csi::Incomplete;
    };
    let events = match final_byte {
        0x75 if params.first() == Some(&b'?') => {
            // Kitty flags: `CSI ? <flags> u`.
            let Some(digits) = params.get(1..) else {
                return Csi::Complete(Vec::new(), end.saturating_add(1));
            };
            if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit()) {
                Vec::new()
            } else {
                let text = String::from_utf8_lossy(digits);
                match text.parse::<u8>() {
                    Ok(flags) => vec![Event::Reply(Reply::KittyFlags(flags))],
                    Err(_) => Vec::new(),
                }
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
            _ => Vec::new(),
        },
        0x46 => {
            if params.is_empty() {
                vec![Event::Key(Key::End)]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };
    Csi::Complete(events, end.saturating_add(1))
}

/// One SS3 parse result.
enum Ss3 {
    /// Parsed events and bytes consumed.
    Complete(Vec<Event>, usize),
    /// `ESC O` with nothing after it yet.
    Incomplete,
}

/// Parses `ESC O` at the start of `buf`.
fn parse_ss3(buf: &[u8]) -> Ss3 {
    let Some(final_byte) = buf.get(2).copied() else {
        return Ss3::Incomplete;
    };
    let events = match final_byte {
        b'F' => vec![Event::Key(Key::End)],
        _ => Vec::new(),
    };
    Ss3::Complete(events, 3)
}

/// One UTF-8 decode result at a position.
enum Char {
    /// A character and its byte length.
    Done(char, usize),
    /// A truncated sequence at the end of the read.
    Incomplete,
    /// An invalid byte; the caller drops one byte.
    Invalid,
}

/// Decodes one character at the start of `buf`.
fn decode_char(buf: &[u8]) -> Char {
    let Some(first) = buf.first().copied() else {
        return Char::Invalid;
    };
    if first < 0x80 {
        let ch = char::from(first);
        return Char::Done(ch, 1);
    }
    let len = if first & 0xe0 == 0xc0 {
        2
    } else if first & 0xf0 == 0xe0 {
        3
    } else if first & 0xf8 == 0xf0 {
        4
    } else {
        return Char::Invalid;
    };
    if buf.len() < len {
        // Truncated only when every byte so far continues correctly;
        // otherwise the lead byte is invalid.
        let Some(tail) = buf.get(1..) else {
            return Char::Incomplete;
        };
        let ok = tail.iter().all(|b| b & 0xc0 == 0x80);
        if ok {
            return Char::Incomplete;
        }
        return Char::Invalid;
    }
    let Some(head) = buf.get(..len) else {
        return Char::Invalid;
    };
    match std::str::from_utf8(head) {
        Ok(text) => {
            let mut chars = text.chars();
            let ch = chars.next().unwrap_or('\u{FFFD}');
            Char::Done(ch, len)
        }
        Err(_) => Char::Invalid,
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
