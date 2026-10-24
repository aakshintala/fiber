//! The hub client's websocket transport (`docs/invocation.md`, "What the hub speaks").

mod frame;
mod handshake;
mod tls;
mod tunnel;

use crate::Error;
use ring::rand::SecureRandom;
use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

/// Connects a hub websocket and exposes its text messages as lines.
pub fn connect(endpoint: &Endpoint) -> Result<UnixStream, Error> {
    connect_with(endpoint, &Via::from_env())
}

/// Connects using an explicit trust, proxy, resolver and timeout policy.
pub fn connect_with(endpoint: &Endpoint, via: &Via) -> Result<UnixStream, Error> {
    let (tcp, tail) = tunnel::connect(endpoint, via)?;
    let mut socket = tls::Socket::new(tcp, tail, endpoint, via)?;
    socket.tcp.set_read_timeout(Some(via.limits.idle()))?;
    socket.tcp.set_write_timeout(Some(via.limits.idle()))?;
    let key = handshake::key()?;
    socket
        .write_all(handshake::request(endpoint, &key).as_bytes())
        .map_err(|e| tunnel::io_error(e, via.limits.idle()))?;
    let tail = handshake::reply(&mut socket, &key).map_err(|e| {
        if let Error::Io(e) = e {
            tunnel::io_error(e, via.limits.idle())
        } else {
            e
        }
    })?;
    socket.tcp.set_read_timeout(None)?;
    bridge(socket, tail, via)
}

fn bridge(mut socket: tls::Socket, tail: Vec<u8>, via: &Via) -> Result<UnixStream, Error> {
    let (client, mut pair) = UnixStream::pair()?;
    pair.set_write_timeout(Some(via.limits.idle()))?;
    let mut pair_in = pair.try_clone()?;
    let mut outgoing = socket.try_clone()?;
    let stop_tcp = socket.tcp.try_clone()?;
    let stop_pair = pair.try_clone()?;
    #[cfg(test)]
    let in_exit = via.exits.clone();
    #[cfg(test)]
    let out_exit = via.exits.clone();
    std::thread::Builder::new()
        .name("net-ws-in".into())
        .spawn(move || {
            let _result = pump_in(&mut socket, tail, &mut pair_in);
            shutdown(&socket.tcp, &pair_in);
            #[cfg(test)]
            exited(in_exit, "net-ws-in");
        })?;
    let spawned = std::thread::Builder::new()
        .name("net-ws-out".into())
        .spawn(move || {
            let _result = pump_out(&mut outgoing, &mut pair);
            shutdown(&outgoing.tcp, &pair);
            #[cfg(test)]
            exited(out_exit, "net-ws-out");
        });
    if let Err(e) = spawned {
        shutdown(&stop_tcp, &stop_pair);
        return Err(Error::Io(e));
    }
    Ok(client)
}

fn pump_in(socket: &mut tls::Socket, tail: Vec<u8>, pair: &mut UnixStream) -> io::Result<()> {
    let mut sender = socket.try_clone()?;
    let mut read = BufReader::new(Cursor::new(tail).chain(socket));
    let mut frames = frame::Reader::default();
    loop {
        match frames.next(&mut read) {
            frame::Incoming::Text(text) => {
                pair.write_all(text.as_bytes())?;
                pair.write_all(b"\n")?;
            }
            frame::Incoming::Ping(payload) => send_frame(&mut sender, 10, &payload)?,
            frame::Incoming::Pong => {}
            frame::Incoming::Close(payload) => {
                send_frame(&mut sender, 8, &payload)?;
                return Ok(());
            }
            frame::Incoming::Fail(code) => {
                send_frame(&mut sender, 8, &code.to_be_bytes())?;
                return Ok(());
            }
            frame::Incoming::Gone => return Ok(()),
        }
    }
}

fn pump_out(socket: &mut tls::Socket, pair: &mut UnixStream) -> io::Result<()> {
    let mut read = BufReader::new(pair);
    let mut line = Vec::new();
    loop {
        line.clear();
        if read.read_until(b'\n', &mut line)? == 0 {
            send_frame(socket, 8, &1000u16.to_be_bytes())?;
            return Ok(());
        }
        let Some(bytes) = line.strip_suffix(b"\n") else {
            send_frame(socket, 8, &1000u16.to_be_bytes())?;
            return Ok(());
        };
        send_frame(socket, 1, String::from_utf8_lossy(bytes).as_bytes())?;
    }
}

fn send_frame(socket: &mut tls::Socket, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let frame = frame::encode(opcode, payload, &mut |mask| {
        ring::rand::SystemRandom::new()
            .fill(mask)
            .map_err(|_| io::Error::other("secure random unavailable"))
    })?;
    socket.write_all(&frame)
}

fn shutdown(tcp: &TcpStream, pair: &UnixStream) {
    // Shutdown needs no TLS or send lock: it wakes the other pump even while
    // that pump is blocked writing to the peer or the caller.
    match tcp.shutdown(Shutdown::Both) {
        Ok(()) | Err(_) => {}
    }
    match pair.shutdown(Shutdown::Both) {
        Ok(()) | Err(_) => {}
    }
    // Linux resets the peer on last close if caller bytes remain queued.
    // Shut down first so draining cannot wait for more input from the caller.
    let mut unread = pair;
    let _result = io::copy(&mut unread, &mut io::sink());
}

pub use tls::client_config;

/// The connection policy for a listed hub.
#[derive(Clone)]
pub struct Via {
    /// TLS trust and client settings.
    pub tls: Arc<rustls::ClientConfig>,
    /// The HTTP CONNECT proxy, if configured.
    pub proxy: Option<ureq::Proxy>,
    /// Per-address connect and per-call idle limits.
    pub limits: crate::Limits,
    /// The name resolver, also used for IP literal hosts.
    pub resolver: Arc<dyn ureq::unversioned::resolver::Resolver>,
    #[cfg(test)]
    pub(crate) pause: Option<Arc<Pause>>,
    #[cfg(test)]
    pub(crate) exits: Option<std::sync::mpsc::Sender<&'static str>>,
}
impl Via {
    /// The OS trust store, proxy environment, default resolver and production limits.
    pub fn from_env() -> Self {
        Self {
            tls: client_config(),
            proxy: ureq::Proxy::try_from_env(),
            limits: crate::LIMITS,
            resolver: Arc::new(ureq::unversioned::resolver::DefaultResolver::default()),
            #[cfg(test)]
            pause: None,
            #[cfg(test)]
            exits: None,
        }
    }
}

#[cfg(test)]
pub(crate) struct Pause {
    held: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
#[cfg(test)]
impl Pause {
    fn wait(&self) {
        match self.held.send(()) {
            Ok(()) | Err(_) => {}
        }
        match support::lock(&self.release).recv() {
            Ok(()) | Err(_) => {}
        }
    }
}
#[cfg(test)]
fn exited(exit: Option<std::sync::mpsc::Sender<&'static str>>, name: &'static str) {
    if let Some(exit) = exit {
        match exit.send(name) {
            Ok(()) | Err(_) => {}
        }
    }
}

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

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod tests;
