//! An installed hub's websocket port (`docs/invocation.md`, "The hub"):
//! `127.0.0.1:<port>`, one line per websocket text message. The acceptor
//! and the per-connection path live here; `listen` binds the local socket.
//!
//! A port connection counts right after its handshake, hears `hub_hello`,
//! then authenticates before anything else: no attention listener, no
//! feed, no rejoin registration until then. Past authentication a socket
//! pair plugs the connection into the served path, which treats it exactly
//! as a local one: a `hub-ws-out` thread sends pair lines as text frames
//! while the connection thread reads frames into the pair.

use std::io::{self, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Wake;
use contract::{CommandId, ErrorCode};
use log::diag::Severity;
use serde_json::{Map, Value};

use crate::connection::{self, Accept, Hub};

use frame::Incoming;

mod frame;
mod handshake;

#[cfg(test)]
use std::sync::MutexGuard;

/// How long the signal path waits for the stopping closes to go out, well
/// inside the 5 s bound from the signal to exit.
pub(crate) const STOP_CLOSE_WAIT: Duration = Duration::from_secs(1);

/// What `unauthenticated` says: the first command is `authenticate` with a
/// held token, or `pair`.
const UNAUTHENTICATED: &str = "The first command on the hub's port is `authenticate` with a device token the hub holds, or `pair`.";

/// What `pairing_failed` says: the code is wrong, used or too old.
const PAIRING_FAILED: &str = "The pairing code is wrong, used or more than 10 minutes old.";

/// `127.0.0.1:<port>`, bound for an installed hub's websockets, and no
/// other address.
pub(crate) struct Listener {
    /// The bound port socket, accepted once the hub serves.
    pub(crate) tcp: TcpListener,
    /// The web page origins whose websockets the hub accepts.
    pub(crate) origins: Arc<[String]>,
}

/// One websocket's shared write end: every frame goes out whole under the
/// sink lock, from the reader, the pump and the signal path.
pub(crate) struct Web {
    sink: Mutex<Sink>,
    control: TcpStream,
    /// True only once the output pump has drained and sent its close.
    drained: AtomicBool,
}

struct Sink {
    stream: TcpStream,
    closed: bool,
}

impl Web {
    /// Writes one text message as a single frame. Fails once the sink is
    /// closed, so nothing is written past a close.
    fn text(&self, bytes: &[u8]) -> io::Result<()> {
        self.write_frame(frame::text_frame(bytes))
    }

    /// Answers a ping with a pong carrying the same payload.
    fn pong(&self, payload: &[u8]) -> io::Result<()> {
        self.write_frame(frame::pong_frame(payload))
    }

    /// Writes one close frame and marks the sink closed: one close per
    /// connection, waited for rather than written beside. A write that
    /// fails still closes the sink: the connection is gone either way.
    fn close(&self, code: u16, reason: &str) {
        send_close(self, &frame::close_frame(code, reason));
    }

    /// Shuts the TCP stream down both ways, without the sink lock: it
    /// unblocks a stalled reader, writer or close attempt.
    pub(crate) fn shut(&self) {
        self.control.shutdown(Shutdown::Both).unwrap_or(());
    }

    /// Whether the output pump finished its drain and close attempt.
    pub(crate) fn drained(&self) -> bool {
        self.drained.load(Ordering::SeqCst)
    }

    /// Writes one frame, unless the sink is closed already.
    fn write_frame(&self, frame: Vec<u8>) -> io::Result<()> {
        let mut sink = connection::lock(&self.sink);
        if sink.closed {
            return Err(io::Error::other("the websocket is closed"));
        }
        sink.stream.write_all(&frame)
    }

    #[cfg(test)]
    fn hold(&self) -> MutexGuard<'_, Sink> {
        connection::lock(&self.sink)
    }
}

#[cfg(test)]
impl Web {
    /// A websocket that stays as drained as built, for the stop answer's
    /// wait: nothing pumps it, so `drained` never flips on its own.
    #[allow(clippy::unwrap_used, reason = "a test fake; a failure is the test's")]
    pub(crate) fn never_drained(drained: bool) -> Arc<Web> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        Arc::new(Web {
            sink: Mutex::new(Sink {
                stream: server,
                closed: false,
            }),
            control: stream,
            drained: AtomicBool::new(drained),
        })
    }
}

/// Writes one close frame and marks the sink closed, once: every path that
/// ends a connection sends its close through here.
fn send_close(web: &Web, frame: &[u8]) {
    let mut sink = connection::lock(&web.sink);
    if sink.closed {
        return;
    }
    sink.stream.write_all(frame).unwrap_or(());
    sink.closed = true;
}

/// Accepts port connections on a detached `hub-port-accept` thread. Each
/// accepted stream gets its own `hub-ws` thread; a thread that never starts
/// drops its stream. An accept error returns once `stop` is set, as the
/// local acceptor does.
pub(crate) fn spawn_acceptor(
    listener: Listener,
    hub: &Arc<Hub>,
    stop: &Arc<AtomicBool>,
) -> io::Result<()> {
    let origins = listener.origins;
    let tcp = listener.tcp;
    let hub = Arc::clone(hub);
    let stop = Arc::clone(stop);
    thread::Builder::new()
        .name("hub-port-accept".to_owned())
        .spawn(move || {
            let hub = Arc::clone(&hub);
            let stop = Arc::clone(&stop);
            loop {
                match tcp.accept() {
                    Ok((stream, _)) => {
                        let for_thread = Arc::clone(&hub);
                        let stop = Arc::clone(&stop);
                        let origins = Arc::clone(&origins);
                        // The closure owns `stream`: on spawn failure it
                        // is dropped with the closure.
                        if thread::Builder::new()
                            .name("hub-ws".to_owned())
                            .spawn(move || serve(stream, for_thread, stop, origins))
                            .is_err()
                        {
                            // Never counted, and no log line to write.
                        }
                    }
                    Err(_) => {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                    }
                }
            }
        })
        .map(|_| ())
}

/// Serves one port connection: the handshake, the count, `hub_hello`, the
/// first command's authentication, then the bridge to the hub. Anything
/// before authentication succeeds is answered and closed here, rolling the
/// count back: no `client_connected` and no `client_disconnected`.
pub(crate) fn serve(
    stream: TcpStream,
    hub: Arc<Hub>,
    stop: Arc<AtomicBool>,
    origins: Arc<[String]>,
) {
    let mut read = match stream.try_clone() {
        Ok(half) => BufReader::new(half),
        Err(_) => return,
    };
    let mut write = match stream.try_clone() {
        Ok(half) => half,
        Err(_) => return,
    };
    let accept = match handshake::read_request(&mut read, &origins) {
        Ok(accept) => accept,
        Err(refusal) => {
            write.write_all(&handshake::refused(refusal)).unwrap_or(());
            stream.shutdown(Shutdown::Both).unwrap_or(());
            return;
        }
    };
    if write.write_all(&handshake::switching(&accept)).is_err() {
        stream.shutdown(Shutdown::Both).unwrap_or(());
        return;
    }
    drop(write);
    let control = match stream.try_clone() {
        Ok(half) => half,
        Err(_) => {
            stream.shutdown(Shutdown::Both).unwrap_or(());
            return;
        }
    };
    let web = Arc::new(Web {
        sink: Mutex::new(Sink {
            stream,
            closed: false,
        }),
        control,
        drained: AtomicBool::new(false),
    });
    match hub.poll_accept_web(&web, &stop) {
        Accept::Counted(n) => serve_client(web, hub, n, read),
        Accept::Dropped | Accept::Exiting => {
            web.shut();
        }
    }
}

/// Serves a counted websocket: `hub_hello`, the first command, then the
/// bridge. Before authentication succeeds the connection carries only the
/// hello, the first answer and a close.
fn serve_client(web: Arc<Web>, hub: Arc<Hub>, n: u64, read: BufReader<TcpStream>) {
    let hello = connection::line(&hub, "hub_hello", connection::hello_payload(&hub));
    if web.text(&hello).is_err() {
        web.shut();
        hub.rollback(n);
        return;
    }
    let mut read = read;
    let mut frames = frame::Reader::new(frame::FIRST_MESSAGE_MAX);
    // Before authentication the connection carries only pings and the
    // first message: the first text frame ends this wait.
    let message = loop {
        match frames.next(&mut read) {
            Incoming::Text(message) => break Some(message),
            Incoming::Ping(payload) => {
                if web.pong(&payload).is_err() {
                    break None;
                }
            }
            Incoming::Pong => {}
            Incoming::Close(close) => {
                send_close(&web, &frame::echo_close(close.map(|(code, _)| code)));
                break None;
            }
            Incoming::Fail(code) => {
                web.close(code, "");
                break None;
            }
            Incoming::Gone => break None,
        }
    };
    let Some(message) = message else {
        web.shut();
        hub.rollback(n);
        return;
    };
    let (id, who, result) = match connection::classify(message.as_bytes()) {
        Ok(line) if line.session_id.is_none() && line.command == "authenticate" => {
            let who = token(&line.args).and_then(|token| hub.pairing.authenticate(&hub, n, token));
            let Some(who) = who else {
                refuse(
                    &web,
                    &hub,
                    n,
                    Some(&line.id),
                    &ErrorCode::Unauthenticated,
                    "unauthenticated",
                    UNAUTHENTICATED,
                );
                return;
            };
            (line.id, who, None)
        }
        Ok(line) if line.session_id.is_none() && line.command == "pair" => {
            match hub.pairing.redeem(&hub, n, &line.args) {
                Ok((token, who)) => {
                    let result = serde_json::json!({"device":who.by(), "token":token.0});
                    (line.id, who, Some(result))
                }
                Err(crate::pairing::Refused::Pairing) => {
                    refuse(
                        &web,
                        &hub,
                        n,
                        Some(&line.id),
                        &ErrorCode::PairingFailed,
                        "pairing_failed",
                        PAIRING_FAILED,
                    );
                    return;
                }
                Err(crate::pairing::Refused::Io(message)) => {
                    web.text(&connection::line(
                        &hub,
                        "command_rejected",
                        connection::rejection(Some(&line.id), &ErrorCode::IoFailed, &message),
                    ))
                    .unwrap_or(());
                    hub.diag.line(Severity::Error, None, "io_failed", &message);
                    web.close(1011, "");
                    web.shut();
                    hub.rollback(n);
                    return;
                }
            }
        }
        Ok(line) => {
            refuse(
                &web,
                &hub,
                n,
                Some(&line.id),
                &ErrorCode::Unauthenticated,
                "unauthenticated",
                UNAUTHENTICATED,
            );
            return;
        }
        Err(id) => {
            refuse(
                &web,
                &hub,
                n,
                id.as_ref(),
                &ErrorCode::Unauthenticated,
                "unauthenticated",
                UNAUTHENTICATED,
            );
            return;
        }
    };
    hub.pairing.attach_web(&who, Arc::clone(&web));
    let mut accepted = Map::from_iter([("command_id".to_owned(), Value::String(id.0))]);
    if let Some(result) = result {
        accepted.insert("result".to_owned(), result);
    }
    if web
        .text(&connection::line(&hub, "command_accepted", accepted))
        .is_err()
    {
        web.shut();
        hub.pairing.forget(&hub, &who);
        hub.rollback(n);
        return;
    }
    let (hub_end, bridge) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(_) => {
            web.close(1011, "");
            web.shut();
            hub.pairing.forget(&hub, &who);
            hub.rollback(n);
            return;
        }
    };
    let served_who = match &who {
        crate::pairing::Who::Local => crate::pairing::Who::Local,
        crate::pairing::Who::Device(p) => crate::pairing::Who::Device(Arc::clone(p)),
    };
    if spawn(&hub, "hub-ws-serve", {
        let hub = Arc::clone(&hub);
        let web = Arc::clone(&web);
        move || connection::serve_authenticated(hub_end, hub, n, served_who, Some(web))
    })
    .is_err()
    {
        bridge.shutdown(Shutdown::Both).unwrap_or(());
        web.close(1011, "");
        web.shut();
        hub.pairing.forget(&hub, &who);
        hub.rollback(n);
        return;
    }
    let pump = match bridge.try_clone() {
        Ok(pump) => pump,
        Err(_) => {
            bridge.shutdown(Shutdown::Both).unwrap_or(());
            web.close(1011, "");
            web.shut();
            hub.pairing.forget(&hub, &who);
            return;
        }
    };
    if spawn(&hub, "hub-ws-out", {
        let web = Arc::clone(&web);
        let tick = Arc::clone(&hub.tick);
        move || {
            pump_lines(&web, pump);
            web.drained.store(true, Ordering::SeqCst);
            tick.wake();
        }
    })
    .is_err()
    {
        bridge.shutdown(Shutdown::Both).unwrap_or(());
        web.close(1011, "");
        web.shut();
        hub.pairing.forget(&hub, &who);
        return;
    }
    read_lines(&web, read, bridge);
}

/// The device token in `args`, when they are exactly one string `token`.
fn token(args: &Map<String, Value>) -> Option<&str> {
    if args.len() != 1 {
        return None;
    }
    args.get("token")?.as_str()
}

/// Answers a refused first command and closes: the answer, close 1008
/// carrying `close_reason`, the shutdown and the rollback. Logs
/// `client_unauthenticated` with no token, code or argument text.
fn refuse(
    web: &Web,
    hub: &Hub,
    n: u64,
    id: Option<&CommandId>,
    code: &ErrorCode,
    close_reason: &str,
    message: &str,
) {
    let payload = connection::rejection(id, code, message);
    web.text(&connection::line(hub, "command_rejected", payload))
        .unwrap_or(());
    web.close(1008, close_reason);
    web.shut();
    hub.diag.line(
        Severity::Info,
        None,
        "client_unauthenticated",
        &format!("Client {n} presented no valid device token."),
    );
    hub.rollback(n);
}

/// Starts a `hub-ws-*` thread. In tests, `Hub::fail_spawn` refuses every
/// spawn of the named thread, so the spawn-failure paths run with no outside
/// seam.
fn spawn(hub: &Hub, name: &'static str, run: impl FnOnce() + Send + 'static) -> io::Result<()> {
    #[cfg(not(test))]
    let _ = hub;
    #[cfg(test)]
    {
        let fail = connection::lock(&hub.fail_spawn);
        if fail.as_deref() == Some(name) {
            return Err(io::Error::other(format!(
                "the test refused to spawn {name}"
            )));
        }
    }
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(run)
        .map(|_| ())
}

/// Reads client frames into the pair: each text message goes in as one
/// line, parsed as JSON first so a raw newline never splits a line. A ping
/// is ponged; anything ending the loop shuts the pair and the TCP stream
/// down.
fn read_lines(web: &Arc<Web>, read: BufReader<TcpStream>, mut bridge: UnixStream) {
    let mut read = read;
    let mut frames = frame::Reader::new(usize::MAX);
    loop {
        match frames.next(&mut read) {
            Incoming::Text(message) => {
                if support::framing::write_line(&mut bridge, &one_line(&message)).is_err() {
                    break;
                }
            }
            Incoming::Ping(payload) => {
                if web.pong(&payload).is_err() {
                    break;
                }
            }
            Incoming::Pong => {}
            Incoming::Close(close) => {
                send_close(web, &frame::echo_close(close.map(|(code, _)| code)));
                break;
            }
            Incoming::Fail(code) => {
                web.close(code, "");
                break;
            }
            Incoming::Gone => break,
        }
    }
    bridge.shutdown(Shutdown::Both).unwrap_or(());
    web.shut();
}

/// One pair line for a text message, without its newline: unchanged when it
/// holds none, else the message parsed as JSON and written compact, so it
/// is one line. Invalid JSON becomes the fixed invalid line `{`, answered
/// `malformed` with no id, as an invalid single line is.
fn one_line(message: &str) -> Vec<u8> {
    if !message.as_bytes().contains(&b'\n') {
        return message.as_bytes().to_vec();
    }
    match serde_json::from_str::<Value>(message) {
        Ok(value) => serde_json::to_vec(&value).unwrap_or_default(),
        Err(_) => b"{".to_vec(),
    }
}

/// Sends pair lines as text messages: one frame per line, without its
/// newline. Ends at EOF or a failed send, with close 1000 unless one was
/// sent, then shuts the pair and the TCP stream down.
fn pump_lines(web: &Arc<Web>, bridge: UnixStream) {
    let mut read = BufReader::new(bridge);
    loop {
        match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => break,
            Ok(Some(buf)) => {
                if web.text(String::from_utf8_lossy(&buf).as_bytes()).is_err() {
                    break;
                }
            }
        }
    }
    web.close(1000, "");
    read.get_ref().shutdown(Shutdown::Both).unwrap_or(());
    web.shut();
}

/// Sends every websocket the stopping close, waiting at most
/// `STOP_CLOSE_WAIT` for the sinks to free, then shuts every stream: a
/// client that reads gets the close even mid-frame, and the hub exits
/// inside the 5 s bound. Returns at once for no websockets.
pub(crate) fn stop_all(hub: &Hub, webs: Vec<Arc<Web>>) {
    if webs.is_empty() {
        return;
    }
    let done = Arc::new(Mutex::new(0usize));
    let total = webs.len();
    for web in &webs {
        let stopped = spawn(hub, "hub-ws-stop", {
            let web = Arc::clone(web);
            let done = Arc::clone(&done);
            let tick = Arc::clone(&hub.tick);
            move || {
                web.close(1001, "The hub is stopping.");
                *connection::lock(&done) += 1;
                tick.wake();
            }
        });
        if stopped.is_err() {
            // A thread that cannot be spawned counts as done at once.
            *connection::lock(&done) += 1;
            hub.tick.wake();
        }
    }
    let start = hub.clock.now();
    let Some(end) = start.checked_add(STOP_CLOSE_WAIT) else {
        for web in &webs {
            web.shut();
        }
        return;
    };
    // Held into the park, as `idle_wait` does: a close attempt finishing
    // between the count check and the park wakes this thread instead of
    // nobody.
    let mut guard = hub.tick.hold();
    loop {
        if *connection::lock(&done) == total {
            break;
        }
        if hub.clock.now() >= end {
            break;
        }
        let parked = hub.tick.park(hub.clock.as_ref(), guard, Some(end));
        let Some(next) = parked else {
            break;
        };
        guard = next;
    }
    for web in &webs {
        web.shut();
    }
}

#[cfg(test)]
#[path = "port_tests.rs"]
mod tests;
