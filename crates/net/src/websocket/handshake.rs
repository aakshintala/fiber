//! The websocket HTTP upgrade and its bounded reply.
use std::io::{self, Read};

use base64::{Engine, prelude::BASE64_STANDARD as STANDARD};
use ring::rand::SecureRandom;

use super::Endpoint;
use crate::Error;

pub(super) fn accept(key: &str) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    digest.update(key.as_bytes());
    digest.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    STANDARD.encode(digest.finish().as_ref())
}

pub(super) fn key() -> io::Result<String> {
    let mut bytes = [0; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("secure random unavailable"))?;
    Ok(STANDARD.encode(bytes))
}

pub(super) fn authority(host: &str, port: u16) -> String {
    format!("{}:{port}", bracketed(host))
}

fn bracketed(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

pub(super) fn request(endpoint: &Endpoint, key: &str) -> String {
    let default = if endpoint.secure { 443 } else { 80 };
    let host = if endpoint.port == default {
        bracketed(&endpoint.host)
    } else {
        authority(&endpoint.host, endpoint.port)
    };
    format!(
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        endpoint.target
    )
}

pub(super) fn reply(read: &mut impl Read, key: &str) -> Result<Vec<u8>, Error> {
    let (headers, tail) = headers(read)?;
    let mut lines = headers.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let mut parts = status.split_whitespace();
    if parts.next() != Some("HTTP/1.1") || parts.next() != Some("101") {
        return Err(Error::Upgrade {
            status_line: status.into(),
        });
    }
    let mut upgrade = false;
    let mut connection = false;
    let mut accepted = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("Upgrade") {
            upgrade = value.eq_ignore_ascii_case("websocket");
        }
        if name.eq_ignore_ascii_case("Connection") {
            connection = value
                .split(',')
                .any(|word| word.trim().eq_ignore_ascii_case("upgrade"));
        }
        if name.eq_ignore_ascii_case("Sec-WebSocket-Accept") {
            accepted = value == accept(key);
        }
    }
    if !upgrade || !connection || !accepted {
        return Err(Error::Accept);
    }
    Ok(tail)
}

pub(super) fn headers(read: &mut impl Read) -> Result<(String, Vec<u8>), Error> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let n = read.read(&mut chunk)?;
        if n == 0 {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "EOF in HTTP reply",
            )));
        }
        bytes.extend_from_slice(chunk.get(..n).unwrap_or_default());
        if let Some(end) = bytes
            .windows(4)
            .position(|word| word == b"\r\n\r\n")
            .map(|n| n + 4)
        {
            if end > 8192 {
                return Err(Error::ReplyTooLong);
            }
            let tail = bytes.split_off(end);
            let text = String::from_utf8(bytes).map_err(|_| Error::Accept)?;
            return Ok((text, tail));
        }
        if bytes.len() > 8192 {
            return Err(Error::ReplyTooLong);
        }
    }
}

#[cfg(test)]
#[path = "handshake_tests.rs"]
mod tests;
