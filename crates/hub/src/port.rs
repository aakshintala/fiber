//! An installed hub's websocket port (`docs/invocation.md`, "The hub"):
 //! `127.0.0.1:<port>`, one line per websocket text message. The acceptor
//! and the per-connection path live here; `listen` binds the local socket.

use std::net::TcpListener;
use std::sync::Arc;

/// `127.0.0.1:<port>`, bound for an installed hub's websockets, and no
/// other address.
pub(crate) struct Listener {
    /// The bound port socket, accepted once the hub serves.
    #[expect(dead_code, reason = "read from Task 4")]
    pub(crate) tcp: TcpListener,
    /// The web page origins whose websockets the hub accepts.
    #[expect(dead_code, reason = "read from Task 4")]
    pub(crate) origins: Arc<[String]>,
}

#[cfg_attr(not(test), expect(dead_code, reason = "used from Task 4"))]
mod frame;
#[cfg_attr(not(test), expect(dead_code, reason = "used from Task 4"))]
mod handshake;
