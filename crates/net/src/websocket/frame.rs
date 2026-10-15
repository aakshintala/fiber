//! Masked client frames and unmasked server frames, with message assembly.
use std::io::{self, BufRead};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Incoming {
    Text(String),
    Ping(Vec<u8>),
    Pong,
    Close(Vec<u8>),
    Fail(u16),
    Gone,
}

#[derive(Default)]
pub(super) struct Reader {
    open: Option<Vec<u8>>,
}

impl Reader {
    pub(super) fn next(&mut self, read: &mut impl BufRead) -> Incoming {
        loop {
            let mut header = [0; 2];
            if read.read_exact(&mut header).is_err() {
                return Incoming::Gone;
            }
            let [first, second] = header;
            if first & 0x70 != 0 {
                return Incoming::Fail(1002);
            }
            if second & 0x80 != 0 {
                return Incoming::Fail(1002);
            }
            let opcode = first & 0x0f;
            if !matches!(opcode, 0 | 1 | 2 | 8 | 9 | 10) {
                return Incoming::Fail(1002);
            }
            let len = match second {
                126 => {
                    let mut bytes = [0; 2];
                    if read.read_exact(&mut bytes).is_err() {
                        return Incoming::Gone;
                    }
                    let len = u64::from(u16::from_be_bytes(bytes));
                    if len < 126 {
                        return Incoming::Fail(1002);
                    }
                    len
                }
                127 => {
                    let mut bytes = [0; 8];
                    if read.read_exact(&mut bytes).is_err() {
                        return Incoming::Gone;
                    }
                    let len = u64::from_be_bytes(bytes);
                    if len < 65_536 {
                        return Incoming::Fail(1002);
                    }
                    if len & (1 << 63) != 0 {
                        return Incoming::Fail(1002);
                    }
                    len
                }
                len => u64::from(len),
            };
            let fin = first & 0x80 != 0;
            if opcode >= 8 {
                if !fin {
                    return Incoming::Fail(1002);
                }
                if len > 125 {
                    return Incoming::Fail(1002);
                }
                let mut payload = Vec::new();
                if !payload_read(read, len, &mut payload) {
                    return Incoming::Gone;
                }
                return match opcode {
                    8 => close(payload),
                    9 => Incoming::Ping(payload),
                    10 => Incoming::Pong,
                    _ => Incoming::Fail(1002),
                };
            }
            if opcode == 0 && self.open.is_none() {
                return Incoming::Fail(1002);
            }
            if opcode != 0 && self.open.is_some() {
                return Incoming::Fail(1002);
            }
            if opcode == 2 {
                return Incoming::Fail(1003);
            }
            let bytes = self.open.get_or_insert_with(Vec::new);
            if !payload_read(read, len, bytes) {
                return Incoming::Gone;
            }
            if !fin {
                continue;
            }
            let bytes = self.open.take().unwrap_or_default();
            return match String::from_utf8(bytes) {
                Ok(text) if !text.contains('\n') => Incoming::Text(text),
                Ok(_) | Err(_) => Incoming::Fail(1007),
            };
        }
    }
}

// Never allocate a peer-declared length before its bytes arrive.
fn payload_read(read: &mut impl BufRead, len: u64, bytes: &mut Vec<u8>) -> bool {
    let mut remaining = len;
    let mut chunk = [0; 4096];
    while remaining != 0 {
        let want = usize::try_from(remaining.min(4096)).unwrap_or(4096);
        let Some(slot) = chunk.get_mut(..want) else {
            return false;
        };
        if read.read_exact(slot).is_err() {
            return false;
        }
        bytes.extend_from_slice(slot);
        remaining -= u64::try_from(want).unwrap_or(4096);
    }
    true
}

fn close(payload: Vec<u8>) -> Incoming {
    if payload.is_empty() {
        return Incoming::Close(payload);
    }
    let Some(pair) = payload.get(..2) else {
        return Incoming::Fail(1002);
    };
    let Ok(pair) = <[u8; 2]>::try_from(pair) else {
        return Incoming::Fail(1002);
    };
    if !matches!(u16::from_be_bytes(pair), 1000..=1003 | 1007..=1014 | 3000..=4999) {
        return Incoming::Fail(1002);
    }
    if std::str::from_utf8(payload.get(2..).unwrap_or_default()).is_err() {
        return Incoming::Fail(1007);
    }
    Incoming::Close(payload)
}

pub(super) fn encode(
    opcode: u8,
    payload: &[u8],
    draw: &mut impl FnMut(&mut [u8; 4]) -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let mut mask = [0; 4];
    draw(&mut mask)?;
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        0..=125 => out.push(0x80 | u8::try_from(payload.len()).unwrap_or(125)),
        126..=65_535 => {
            out.push(0xfe);
            out.extend_from_slice(&u16::try_from(payload.len()).unwrap_or(65_535).to_be_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(
                &u64::try_from(payload.len())
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
        }
    }
    out.extend_from_slice(&mask);
    out.extend(
        payload
            .iter()
            .zip(mask.iter().cycle())
            .map(|(byte, mask)| byte ^ mask),
    );
    Ok(out)
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
