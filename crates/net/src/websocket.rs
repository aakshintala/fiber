//! The hub client's websocket transport (`docs/invocation.md`, "What the hub speaks").

#[cfg(test)]
mod frame;
#[cfg(test)]
mod handshake;

/// The parsed address of a hub's websocket, without credentials.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Whether to negotiate TLS using the host for verification.
    pub secure: bool,
    /// A DNS name or an IP literal, without IPv6 brackets.
    pub host: String,
    /// The TCP port.
    pub port: u16,
    /// The HTTP request target.
    pub target: String,
}
