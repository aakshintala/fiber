use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ureq::Timeout;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::time::Duration;
use ureq::unversioned::transport::{LazyBuffers, NextTimeout, Transport};

use super::{Socket, open};
use crate::{Error, Keep};

fn wait() -> NextTimeout {
    NextTimeout {
        after: Duration::NotHappening,
        reason: Timeout::Global,
    }
}

/// Whether `error` is an open failing on a stopped call.
fn is_stopped(error: &io::Error) -> bool {
    matches!(
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Error>()),
        Some(Error::Stopped)
    )
}

#[test]
fn a_socket_is_open_until_a_read_finds_the_peer_closed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (stream, mut peer) = fakes::within(
        "the socket to connect and the listener to accept it",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            let stream = TcpStream::connect(addr).unwrap();
            let (peer, _) = listener.accept().unwrap();
            (stream, peer)
        },
    );
    let mut socket = Socket {
        stream,
        buffers: LazyBuffers::new(1024, 1024),
        open: true,
    };
    assert!(socket.is_open());

    peer.write_all(b"x").unwrap();
    let (mut socket, arrived) = fakes::within(
        "the socket to see the peer's byte",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            let arrived = socket.await_input(wait());
            (socket, arrived)
        },
    );
    assert!(arrived.unwrap());
    assert!(socket.is_open(), "a read that got bytes leaves it open");

    drop(peer);
    let (mut socket, arrived) = fakes::within(
        "the socket to see the peer close",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            let arrived = socket.await_input(wait());
            (socket, arrived)
        },
    );
    assert!(!arrived.unwrap());
    assert!(!socket.is_open(), "a read that found the peer closed");
}

/// A keep stopped before the open starts.
#[derive(Debug, Default)]
struct AlreadyStopped;

impl Keep for AlreadyStopped {
    fn keep(&self, _socket: &TcpStream) -> io::Result<()> {
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        true
    }
}

#[test]
fn an_open_that_starts_stopped_connects_nowhere() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let error = fakes::within(
        "a stopped open to fail",
        fakes::MUST_SUCCEED_WITHIN,
        move || open(&[addr], &AlreadyStopped).unwrap_err(),
    );
    assert!(is_stopped(&error), "a stopped open fails stopped: {error}");
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "a stopped open connects nowhere"
    );
}

#[test]
fn an_open_walks_past_a_refused_address() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let refused = SocketAddr::from(fakes::refused::ADDR);
    let stream = fakes::within(
        "an open past a refused address",
        fakes::MUST_SUCCEED_WITHIN,
        move || open(&[refused, addr], &()).unwrap(),
    );
    assert_eq!(
        stream.peer_addr().unwrap(),
        addr,
        "the open reached the listener past the refused address"
    );
    let (peer, _) = fakes::within(
        "the listener to see the open",
        fakes::MUST_SUCCEED_WITHIN,
        move || listener.accept().unwrap(),
    );
    assert_eq!(
        peer.peer_addr().unwrap(),
        stream.local_addr().unwrap(),
        "the listener saw the open"
    );
}

/// A keep that lets the first stop check pass and stops every later one, so
/// an open over two addresses fails its first address and never tries the
/// second.
#[derive(Debug, Default)]
struct StopsAfterFirstCheck {
    seen: AtomicBool,
}

impl Keep for StopsAfterFirstCheck {
    fn keep(&self, _socket: &TcpStream) -> io::Result<()> {
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.seen.swap(true, Ordering::SeqCst)
    }
}

#[test]
fn an_open_stops_before_its_remaining_addresses() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let refused = SocketAddr::from(fakes::refused::ADDR);
    let error = fakes::within(
        "an open stopped mid-walk to fail",
        fakes::MUST_SUCCEED_WITHIN,
        move || open(&[refused, addr], &StopsAfterFirstCheck::default()).unwrap_err(),
    );
    assert!(
        is_stopped(&error),
        "an open stopped mid-walk fails stopped: {error}"
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "the listener saw no connection"
    );
}

/// A keep that refuses every socket it is handed.
#[derive(Debug, Default)]
struct Refuses;

impl Keep for Refuses {
    fn keep(&self, _socket: &TcpStream) -> io::Result<()> {
        Err(io::Error::other("a keep that refuses for the test"))
    }

    fn is_stopped(&self) -> bool {
        false
    }
}

#[test]
fn a_call_a_keep_refuses_fails() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = server.url();
    let error = fakes::within(
        "a refused call to fail",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            let agent = crate::agent(
                crate::config().proxy(None).build(),
                Arc::new(Refuses),
                DefaultResolver::default(),
            );
            agent.get(url).call().unwrap_err()
        },
    );
    // The refusal surfaces as the open failing, not as a status or a
    // timeout.
    let ureq::Error::Io(refused) = &error else {
        panic!("a refused keep fails the open as io: {error}");
    };
    assert_eq!(refused.to_string(), "a keep that refuses for the test");
}

#[test]
fn an_agent_over_the_shared_connector_gets_a_response() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = server.url();
    let text = fakes::within(
        "an agent GET to answer",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            let agent = crate::agent(
                crate::config().proxy(None).build(),
                Arc::new(()),
                DefaultResolver::default(),
            );
            let mut body = agent.get(url).call().unwrap().into_body().into_reader();
            let mut text = String::new();
            body.read_to_string(&mut text).unwrap();
            text
        },
    );
    assert_eq!(text, "{}", "the agent read the fake's response");
    assert_eq!(server.requests().len(), 1, "the fake saw the GET");
}

/// A keep that holds a handle to the socket it was handed.
#[derive(Debug, Default)]
struct Recording {
    kept: Mutex<Option<TcpStream>>,
}

impl Keep for Recording {
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        *self.kept.lock().unwrap() = Some(socket.try_clone()?);
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        false
    }
}

#[test]
fn the_connector_hands_the_keep_its_socket() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = server.url();
    let keep = Arc::new(Recording::default());
    let worker = Arc::clone(&keep);
    fakes::within(
        "a recording GET to answer",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            let agent = crate::agent(
                crate::config().proxy(None).build(),
                worker,
                DefaultResolver::default(),
            );
            let mut body = agent.get(url).call().unwrap().into_body().into_reader();
            body.read_to_string(&mut String::new()).unwrap();
        },
    );
    let kept = keep.kept.lock().unwrap();
    let kept = kept.as_ref().expect("the keep was handed the socket");
    let port: u16 = server
        .url()
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap();
    assert_eq!(
        kept.peer_addr().unwrap().port(),
        port,
        "the handed socket talks to the fake"
    );
}
