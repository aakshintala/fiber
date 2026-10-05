//! A fake HTTP CONNECT proxy (`docs/dependencies.md`, "Proxies"): it answers
//! `CONNECT host:port` by dialling the target and copying bytes both ways, so
//! a test can prove a client tunnelled through it. Anything else gets a 405.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Default)]
struct State {
    /// Each `CONNECT` target in arrival order, recorded before the 200 is
    /// sent, so a client holding the tunnel sees it recorded.
    connects: Vec<String>,
    /// Connections whose client side closed first, in arrival order.
    closed: usize,
    stopping: bool,
}

/// A fake CONNECT proxy listening on a local port. Dropping it closes the
/// port.
pub struct ConnectProxy {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    arrived: Arc<Condvar>,
    accept: Option<JoinHandle<()>>,
}

impl ConnectProxy {
    /// Listens on a free port on 127.0.0.1. The port accepts connections
    /// when this returns.
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let arrived = Arc::new(Condvar::new());
        let shared = Arc::clone(&state);
        let wake = Arc::clone(&arrived);
        let accept = thread::Builder::new()
            .name("fake-connect-proxy".to_owned())
            .spawn(move || accept_loop(&listener, &shared, &wake))
            .map_err(io::Error::other)?;
        Ok(Self {
            addr,
            state,
            arrived,
            accept: Some(accept),
        })
    }

    /// The proxy URL to hand a client, such as `http://127.0.0.1:49152`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The local port the proxy listens on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Every `CONNECT` target received so far, as `host:port`, in arrival
    /// order.
    pub fn connects(&self) -> Vec<String> {
        lock(&self.state).connects.clone()
    }

    /// Waits, at most `within` of real time, until at least `count`
    /// `CONNECT` targets are recorded. True once they are; false at the
    /// deadline.
    pub fn await_connects(&self, count: usize, within: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.connects.len() < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.connects.len() >= count
    }

    /// Waits, at most `within` of real time, until at least `count`
    /// connections saw their client side close. True once they did; false
    /// at the deadline.
    pub fn await_closed(&self, count: usize, within: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.closed < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.closed >= count
    }
}

impl Drop for ConnectProxy {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.state);
            state.stopping = true;
        }
        self.arrived.notify_all();
        // A connection wakes the accept thread to see `stopping`. If none can
        // be made the thread is left blocked rather than joined forever.
        if TcpStream::connect(self.addr).is_ok()
            && let Some(accept) = self.accept.take()
        {
            // Builds abort on panic (docs/code-quality.md, "Panics"), so a
            // join never carries one.
            match accept.join() {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// A lock that outlives a panicked holder: the state is plain data, and a
/// test thread that panicked has already failed its test.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn accept_loop(listener: &TcpListener, state: &Arc<Mutex<State>>, arrived: &Arc<Condvar>) {
    for stream in listener.incoming() {
        if lock(state).stopping {
            return;
        }
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(state);
        let arrived = Arc::clone(arrived);
        // One thread per connection, so a tunnel held open never stalls
        // another.
        let spawned = thread::Builder::new()
            .name("fake-connect-proxy-conn".to_owned())
            .spawn(move || {
                // A connection that breaks mid-handshake has already failed
                // its client, and there is no tunnel to run.
                match serve(stream, &state, &arrived) {
                    Ok(()) | Err(_) => {}
                }
            });
        // Out of threads: the dropped connection fails its client.
        if spawned.is_err() {
            continue;
        }
    }
}

fn serve(client: TcpStream, state: &Mutex<State>, arrived: &Condvar) -> io::Result<()> {
    let mut reader = BufReader::new(client);
    let mut line = String::new();
    // Zero bytes is the `Drop` wake-up, or a client that went away: there
    // is no request to answer.
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let (method, target) = {
        let mut words = line.split_whitespace();
        (
            words.next().unwrap_or("").to_owned(),
            words.next().unwrap_or("").to_owned(),
        )
    };
    // The rest of the head is skipped: the proxy needs only the target.
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid("headers end before a blank line"));
        }
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }
    let mut client = reader.into_inner();
    if method != "CONNECT" {
        let reply =
            "HTTP/1.1 405 Method Not Allowed\r\nconnection: close\r\ncontent-length: 0\r\n\r\n";
        client.write_all(reply.as_bytes())?;
        client.flush()?;
        return Ok(());
    }
    if !is_host_port(&target) {
        let reply = "HTTP/1.1 400 Bad Request\r\nconnection: close\r\ncontent-length: 0\r\n\r\n";
        client.write_all(reply.as_bytes())?;
        client.flush()?;
        return Ok(());
    }
    let origin = match TcpStream::connect(target.as_str()) {
        Ok(origin) => origin,
        Err(_) => {
            let reply =
                "HTTP/1.1 502 Bad Gateway\r\nconnection: close\r\ncontent-length: 0\r\n\r\n";
            client.write_all(reply.as_bytes())?;
            client.flush()?;
            return Ok(());
        }
    };
    {
        lock(state).connects.push(target.to_owned());
        arrived.notify_all();
    }
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    client.flush()?;
    tunnel(client, origin, state, arrived);
    Ok(())
}

/// Whether `target` is shaped like `host:port`, so no dial can turn into a
/// DNS lookup.
fn is_host_port(target: &str) -> bool {
    target
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// Copies bytes both ways until either side closes, then shuts the other
/// half down so its blocked read ends too.
fn tunnel(client: TcpStream, origin: TcpStream, state: &Mutex<State>, arrived: &Condvar) {
    let Ok(client_read) = client.try_clone() else {
        return;
    };
    let Ok(origin_read) = origin.try_clone() else {
        return;
    };
    let closer = TunnelCloser {
        client,
        origin,
        state,
        arrived,
    };
    // The serving thread pumps one direction; one more thread pumps the
    // other. Both are joined, so no thread outlives the tunnel.
    let other = thread::Builder::new()
        .name("fake-connect-proxy-pump".to_owned())
        .spawn(move || {
            let mut read = origin_read;
            let mut write = closer.client;
            let _copied = io::copy(&mut read, &mut write);
            let _closed = read.shutdown(Shutdown::Both);
            let _closed = write.shutdown(Shutdown::Both);
        });
    let mut read = client_read;
    let mut write = closer.origin;
    let copied = io::copy(&mut read, &mut write);
    // A clean end of the client side is the client going away, such as a
    // cancelled call closing its socket.
    if copied.is_ok() {
        lock(closer.state).closed += 1;
        closer.arrived.notify_all();
    }
    let _closed = read.shutdown(Shutdown::Both);
    let _closed = write.shutdown(Shutdown::Both);
    if let Ok(other) = other {
        match other.join() {
            Ok(()) | Err(_) => {}
        }
    }
}

/// The two handles a pump's shutdown closes. Only the counts live past the
/// pumps.
struct TunnelCloser<'a> {
    client: TcpStream,
    origin: TcpStream,
    state: &'a Mutex<State>,
    arrived: &'a Condvar,
}

#[cfg(test)]
#[path = "connect_proxy_tests.rs"]
mod tests;
