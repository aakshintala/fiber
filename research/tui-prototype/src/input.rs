//! The terminal's input, read and parsed here rather than by crossterm.
//! crossterm parses the kitty flags reply and the primary device attributes
//! reply, but keeps both private to its blocking `supports_keyboard_enhancement()`;
//! `event::read()` never returns them. Detection that does not block the first
//! frame has to see them in the ordinary input stream, so this module reads it.

use std::io;
use std::sync::atomic::{AtomicI32, Ordering::Relaxed};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Other,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub sup: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mouse {
    Down,
    Up,
    Drag,
    WheelUp,
    WheelDown,
    Other,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ev {
    Key(Key, Mods),
    /// kind, column, row (both from 0), modifiers
    Mouse(Mouse, u16, u16, Mods),
    /// the reply to `CSI ? u`: the kitty keyboard flags in force
    KittyFlags(u32),
    /// the reply to `CSI c`
    Da1,
}

fn kitty_mods(n: u32) -> Mods {
    let b = n.saturating_sub(1);
    Mods { shift: b & 1 != 0, alt: b & 2 != 0, ctrl: b & 4 != 0, sup: b & 8 != 0 }
}
fn num(s: &str) -> u32 {
    s.split(':').next().unwrap_or("").parse().unwrap_or(1)
}

/// Parses one event from the front of `b`. `None` means the bytes so far are
/// an incomplete sequence; `Some((None, n))` means `n` bytes were skipped.
pub fn parse(b: &[u8]) -> Option<(Option<Ev>, usize)> {
    let k = |key, n| Some((Some(Ev::Key(key, Mods::default())), n));
    let c0 = *b.first()?;
    match c0 {
        0x1b => {
            let c1 = *b.get(1)?;
            match c1 {
                b'[' => csi(b),
                b'O' => {
                    let c2 = *b.get(2)?;
                    let key = match c2 {
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        b'C' => Key::Right,
                        b'D' => Key::Left,
                        b'H' => Key::Home,
                        b'F' => Key::End,
                        _ => Key::Other,
                    };
                    k(key, 3)
                }
                // Esc-prefixed: what a terminal sends for Alt (⌥ on macOS, where configured)
                _ => {
                    let (ev, n) = parse(&b[1..])?;
                    let ev = match ev {
                        Some(Ev::Key(key, mut m)) => {
                            m.alt = true;
                            Some(Ev::Key(key, m))
                        }
                        e => e,
                    };
                    Some((ev, n + 1))
                }
            }
        }
        b'\r' => k(Key::Enter, 1),
        b'\t' => k(Key::Tab, 1),
        0x7f | 0x08 => k(Key::Backspace, 1),
        0x01..=0x1a => Some((Some(Ev::Key(Key::Char((b'a' + c0 - 1) as char), Mods { ctrl: true, ..Default::default() })), 1)),
        0x00..=0x1f => Some((None, 1)),
        _ => {
            let n = match c0 {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            if b.len() < n {
                return None;
            }
            match std::str::from_utf8(&b[..n]).ok().and_then(|s| s.chars().next()) {
                Some(ch) => k(Key::Char(ch), n),
                None => Some((None, 1)),
            }
        }
    }
}

fn csi(b: &[u8]) -> Option<(Option<Ev>, usize)> {
    let end = match b[2..].iter().position(|c| (0x40..=0x7e).contains(c)) {
        Some(i) => i + 2,
        None if b.len() > 64 => return Some((None, 1)),
        None => return None,
    };
    let n = end + 1;
    let params = std::str::from_utf8(&b[2..end]).unwrap_or("");
    let fin = b[end];
    let key = |key, m| Some((Some(Ev::Key(key, m)), n));
    if let Some(p) = params.strip_prefix('<') {
        // SGR mouse: button;column;row, M for press or drag, m for release
        let v: Vec<u32> = p.split(';').map(|x| x.parse().unwrap_or(0)).collect();
        if v.len() < 3 {
            return Some((None, n));
        }
        let (cb, x, y) = (v[0], v[1].saturating_sub(1) as u16, v[2].saturating_sub(1) as u16);
        let m = Mods { shift: cb & 4 != 0, alt: cb & 8 != 0, ctrl: cb & 16 != 0, sup: false };
        // wheel buttons 4 to 7: up, down, left, right. A trackpad's sideways drift
        // arrives as 6 and 7 (66, 67), which are not vertical scrolling
        let kind = if cb & 64 != 0 {
            match cb & 3 {
                0 => Mouse::WheelUp,
                1 => Mouse::WheelDown,
                _ => Mouse::Other,
            }
        } else if cb & 3 != 0 {
            Mouse::Other
        } else if fin == b'm' {
            Mouse::Up
        } else if cb & 32 != 0 {
            Mouse::Drag
        } else {
            Mouse::Down
        };
        return Some((Some(Ev::Mouse(kind, x, y, m)), n));
    }
    if let Some(p) = params.strip_prefix('?') {
        return Some((
            match fin {
                b'u' => Some(Ev::KittyFlags(p.parse().unwrap_or(0))),
                b'c' => Some(Ev::Da1),
                _ => None,
            },
            n,
        ));
    }
    let parts: Vec<&str> = params.split(';').collect();
    let mods = parts.get(1).map_or(Mods::default(), |m| kitty_mods(num(m)));
    // a key release, reported only under flag 2, which this program never asks for
    if parts.get(1).is_some_and(|m| m.split(':').nth(1) == Some("3")) {
        return Some((None, n));
    }
    match fin {
        b'u' => {
            let code = num(parts[0]);
            let kk = match code {
                13 => Key::Enter,
                9 => Key::Tab,
                127 | 8 => Key::Backspace,
                27 => Key::Esc,
                c => char::from_u32(c).filter(|c| !c.is_control() && (*c as u32) < 57344).map_or(Key::Other, Key::Char),
            };
            key(kk, mods)
        }
        b'A' => key(Key::Up, mods),
        b'B' => key(Key::Down, mods),
        b'C' => key(Key::Right, mods),
        b'D' => key(Key::Left, mods),
        b'H' => key(Key::Home, mods),
        b'F' => key(Key::End, mods),
        b'Z' => key(Key::BackTab, Mods { shift: true, ..mods }),
        b'~' => {
            let kk = match num(parts[0]) {
                1 | 7 => Key::Home,
                4 | 8 => Key::End,
                3 => Key::Delete,
                5 => Key::PageUp,
                6 => Key::PageDown,
                // xterm's modifyOtherKeys form: 27;mods;code~
                27 => match parts.get(2).map(|c| num(c)) {
                    Some(13) => Key::Enter,
                    Some(9) => Key::Tab,
                    Some(27) => Key::Esc,
                    Some(c) => char::from_u32(c).map_or(Key::Other, Key::Char),
                    None => Key::Other,
                },
                _ => Key::Other,
            };
            key(kk, mods)
        }
        _ => Some((None, n)),
    }
}

// ------------------------------------------------------------ reading
static WINCH_FD: AtomicI32 = AtomicI32::new(-1);
extern "C" fn on_winch(_: libc::c_int) {
    let fd = WINCH_FD.load(Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, b"w".as_ptr().cast(), 1) };
    }
}

pub struct Reader {
    buf: Vec<u8>,
    /// the bytes the last `wait` read, for `--log-input`
    pub raw: Vec<u8>,
    winch: i32,
    /// when the buffer last stopped on an incomplete sequence
    stuck: Option<Instant>,
}
/// How long a lone Esc waits for the rest of a sequence before it counts as the Esc key.
pub const ESC_WAIT: Duration = Duration::from_millis(30);

impl Reader {
    pub fn new() -> io::Result<Reader> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
        }
        WINCH_FD.store(fds[1], Relaxed);
        unsafe { libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t) };
        Ok(Reader { buf: vec![], raw: vec![], winch: fds[0], stuck: None })
    }
    /// The deadline by which an incomplete Esc sequence is flushed as the Esc key.
    pub fn deadline(&self) -> Option<Instant> {
        self.stuck.map(|t| t + ESC_WAIT)
    }
    /// Waits up to `timeout` for input. Returns the events and whether the window was resized.
    pub fn wait(&mut self, timeout: Option<Duration>) -> io::Result<(Vec<Ev>, bool)> {
        let mut p = [libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: self.winch, events: libc::POLLIN, revents: 0 }];
        let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
        let rc = unsafe { libc::poll(p.as_mut_ptr(), 2, ms) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted { Ok((vec![], false)) } else { Err(e) };
        }
        let mut resized = false;
        self.raw.clear();
        if p[1].revents & libc::POLLIN != 0 {
            let mut d = [0u8; 64];
            while unsafe { libc::read(self.winch, d.as_mut_ptr().cast(), d.len()) } > 0 {}
            resized = true;
        }
        if p[0].revents & libc::POLLIN != 0 {
            let mut d = [0u8; 4096];
            let n = unsafe { libc::read(0, d.as_mut_ptr().cast(), d.len()) };
            if n > 0 {
                self.buf.extend_from_slice(&d[..n as usize]);
                self.raw.extend_from_slice(&d[..n as usize]);
                self.stuck = None;
            }
        }
        let mut out = vec![];
        loop {
            match parse(&self.buf) {
                Some((ev, n)) => {
                    self.buf.drain(..n);
                    out.extend(ev);
                }
                None if self.buf.is_empty() => break,
                None => {
                    let t = *self.stuck.get_or_insert_with(Instant::now);
                    if t.elapsed() < ESC_WAIT {
                        break;
                    }
                    // nothing completed it: a lone Esc is the Esc key, anything else is dropped
                    if self.buf[0] == 0x1b {
                        out.push(Ev::Key(Key::Esc, Mods::default()));
                    }
                    self.buf.drain(..1);
                    self.stuck = None;
                }
            }
        }
        Ok((out, resized))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn one(b: &[u8]) -> Option<Ev> {
        parse(b).unwrap().0
    }
    #[test]
    fn replies_and_keys() {
        assert_eq!(one(b"\x1b[?1u"), Some(Ev::KittyFlags(1)));
        assert_eq!(one(b"\x1b[?62;22c"), Some(Ev::Da1));
        assert_eq!(one(b"\x1b[13;2u"), Some(Ev::Key(Key::Enter, Mods { shift: true, ..Default::default() })));
        assert_eq!(one(b"\x1b[102;9u"), Some(Ev::Key(Key::Char('f'), Mods { sup: true, ..Default::default() })));
        assert_eq!(one(b"\x06"), Some(Ev::Key(Key::Char('f'), Mods { ctrl: true, ..Default::default() })));
        assert_eq!(one(b"\x1b[1;3A"), Some(Ev::Key(Key::Up, Mods { alt: true, ..Default::default() })));
        assert_eq!(one(b"\x1bx"), Some(Ev::Key(Key::Char('x'), Mods { alt: true, ..Default::default() })));
        assert_eq!(one(b"\x1b[<0;5;3M"), Some(Ev::Mouse(Mouse::Down, 4, 2, Mods::default())));
        assert_eq!(one(b"\x1b[<32;6;3M"), Some(Ev::Mouse(Mouse::Drag, 5, 2, Mods::default())));
        assert_eq!(one(b"\x1b[<0;6;3m"), Some(Ev::Mouse(Mouse::Up, 5, 2, Mods::default())));
        assert_eq!(one(b"\x1b[<64;5;3M"), Some(Ev::Mouse(Mouse::WheelUp, 4, 2, Mods::default())));
        assert_eq!(one(b"\x1b[<65;5;3M"), Some(Ev::Mouse(Mouse::WheelDown, 4, 2, Mods::default())));
        // sideways wheel, as a trackpad's drift sends it: not up and down
        assert_eq!(one(b"\x1b[<66;5;3M"), Some(Ev::Mouse(Mouse::Other, 4, 2, Mods::default())));
        assert_eq!(one(b"\x1b[<67;5;3M"), Some(Ev::Mouse(Mouse::Other, 4, 2, Mods::default())));
        assert_eq!(parse(b"\x1b"), None);
        assert_eq!(parse(b"\x1b[1;"), None);
        assert_eq!(one("é".as_bytes()), Some(Ev::Key(Key::Char('é'), Mods::default())));
    }
}
