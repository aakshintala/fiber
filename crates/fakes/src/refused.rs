//! A local address whose connection is refused and that no concurrent test
//! can take (`docs/testing.md`, "Fakes").
//!
//! Binding a port, reading it and dropping the socket leaves the port free
//! for any listener that binds port 0 meanwhile, and a fake server in the same
//! binary then answers the connection. This address is loopback port 1: the
//! kernel never hands it to a bind of port 0, which draws from the ephemeral
//! range, and nothing listens there, so a connect is reset on Linux and macOS.

use std::net::{Ipv4Addr, SocketAddrV4};

/// The port connecting to which is refused.
pub const PORT: u16 = 1;

/// The address a connect to is refused.
pub const ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, PORT);

/// `http://127.0.0.1:1`, the base URL of a service that is not running.
pub fn url() -> String {
    format!("http://{ADDR}")
}

#[cfg(test)]
#[path = "refused_tests.rs"]
mod tests;
