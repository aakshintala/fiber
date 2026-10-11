//! An installed hub's websocket port (`docs/invocation.md`, "The hub"):
 //! `127.0.0.1:<port>`, one line per websocket text message. The acceptor
//! and the per-connection path live here; `listen` binds the local socket.

use std::net::TcpListener;
use std::sync::Arc;

/// `127.0.0.1:<port>`, bound for an installed hub's websockets, and no
/// other address.
pub(crate) struct Listener {
    /// The bound port socket, accepted once the hub serves.
    pub(crate) tcp: TcpListener,
    /// The web page origins whose websockets the hub accepts.
    pub(crate) origins: Arc<[String]>,
}
