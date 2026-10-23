//! Pairing codes and terminal QR output (`docs/invocation.md`, "Remote clients").
use crate::{CliError, finish, sessions::request_value, write_line};
use contract::commands::hub::{Device, HubCommand};
use qrcode::{QrCode, render::unicode::Dense1x2};
use serde_json::Value;
use std::io::{self, BufReader, IsTerminal as _, Write};

const ADDRESS_RULE: &str = "The address must be a ws:// or wss:// URL.";

/// A websocket URL carried unchanged in the pairing QR code.
#[derive(Clone, Debug)]
pub struct Address(String);
impl Address {
    /// Accepts a websocket scheme, a host and an optional port in 1..=65535.
    pub fn parse(value: &str) -> Result<Self, String> {
        let valid = || {
            let (scheme, rest) = value.split_once("://")?;
            if !scheme.eq_ignore_ascii_case("ws") && !scheme.eq_ignore_ascii_case("wss") {
                return None;
            }
            let authority = rest.split(['/', '?', '#']).next()?;
            if authority
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            {
                return None;
            }
            let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
                let (host, tail) = bracketed.split_once(']')?;
                let port = if tail.is_empty() {
                    None
                } else {
                    Some(tail.strip_prefix(':')?)
                };
                (host, port)
            } else {
                match authority.rsplit_once(':') {
                    Some((host, port)) => (host, Some(port)),
                    None => (authority, None),
                }
            };
            if host.is_empty() {
                return None;
            }
            if let Some(port) = port
                && (port.is_empty()
                    || !port.bytes().all(|b| b.is_ascii_digit())
                    || port.parse::<u16>().ok()? == 0)
            {
                return None;
            }
            Some(())
        };
        valid().ok_or_else(|| ADDRESS_RULE.to_owned())?;
        Ok(Self(value.to_owned()))
    }
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Prints a single-use pairing code, with a QR code only on a terminal.
pub fn hub_pair(
    device: &str,
    address: Option<&Address>,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> i32 {
    let mut stdout = io::stdout();
    let terminal = stdout.is_terminal();
    finish(run(&mut stdout, device, address, terminal, connect))
}
fn run(
    out: &mut dyn Write,
    device: &str,
    address: Option<&Address>,
    terminal: bool,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<(), CliError> {
    let (stream, _) = connect().map_err(CliError::Hub)?;
    let mut read = BufReader::new(stream);
    let answer = request_value(
        &mut read,
        "c_pairing_code",
        HubCommand::PairingCode(Device {
            device: device.to_owned(),
        }),
    )?;
    let code = answer
        .get("code")
        .and_then(Value::as_str)
        .ok_or(CliError::PairingAnswerMissing)?;
    // Draw before printing: an oversized address leaves no partial output.
    let qr = if terminal {
        Some(draw(&qr_text(address, code))?)
    } else {
        None
    };
    let print = || -> Result<(), CliError> {
        write_line(out, code)?;
        if let Some(qr) = qr {
            write_line(out, &format!("\n{qr}"))?;
        }
        Ok(())
    };
    print()
}
fn qr_text(address: Option<&Address>, code: &str) -> String {
    address.map_or_else(
        || code.to_owned(),
        |address| format!("{}\n{code}", address.as_str()),
    )
}
fn draw(text: &str) -> Result<String, CliError> {
    let code = QrCode::new(text.as_bytes()).map_err(|_| CliError::QrTooLong)?;
    Ok(code
        .render::<Dense1x2>()
        .quiet_zone(true)
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build())
}
#[cfg(test)]
#[path = "hub_pair_tests.rs"]
mod tests;
