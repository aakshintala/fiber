use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ureq::Timeout;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::time::Duration;
use ureq::unversioned::transport::{LazyBuffers, NextTimeout, Transport};

use super::{Socket, connect_bound, connect_error, open, read_retrying};
use crate::{Error, Keep, LIMITS, Limits};

fn wait() -> NextTimeout {
    NextTimeout {
        after: Duration::NotHappening,
        reason: Timeout::Global,
    }
}

/// Whether `error` is an open failing on a stopped call.
fn is_stopped(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::Io(stopped)
            if matches!(
                stopped
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<Error>()),
                Some(Error::Stopped)
            )
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
        idle: std::time::Duration::from_secs(300),
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
        move || open(&[addr], &AlreadyStopped, LIMITS, wait()).unwrap_err(),
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
        move || open(&[refused, addr], &(), LIMITS, wait()).unwrap(),
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
        move || {
            open(
                &[refused, addr],
                &StopsAfterFirstCheck::default(),
                LIMITS,
                wait(),
            )
            .unwrap_err()
        },
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
                LIMITS,
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
                LIMITS,
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
                LIMITS,
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

/// One test's wall-clock bound. Every wait names it.
const EACH_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

/// The short idle the stall tests inject.
const IDLE_200MS: std::time::Duration = std::time::Duration::from_millis(200);

fn short_limits() -> Limits {
    Limits::new(std::time::Duration::from_secs(1), IDLE_200MS).expect("short limits build")
}

fn ureq_after(after: Duration, reason: Timeout) -> NextTimeout {
    NextTimeout { after, reason }
}

/// A connected client socket and its silent peer. The peer never sends, so
/// a read with a short idle times out.
fn silent_pair(idle: std::time::Duration) -> (Socket, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (stream, peer) = fakes::within("the stall pair to connect", EACH_WITHIN, move || {
        let stream = TcpStream::connect(addr).unwrap();
        let (peer, _) = listener.accept().unwrap();
        (stream, peer)
    });
    peer.set_read_timeout(Some(EACH_WITHIN)).unwrap();
    (
        Socket {
            stream,
            buffers: LazyBuffers::new(1024, 1024),
            open: true,
            idle,
        },
        peer,
    )
}

/// The stall's idle when `error` carries one.
fn stalled_idle(error: &ureq::Error) -> Option<std::time::Duration> {
    let ureq::Error::Io(stalled) = error else {
        return None;
    };
    let inner = stalled
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<Error>())?;
    match inner {
        Error::Stalled { idle } => Some(*idle),
        Error::Stopped | Error::ConnectTimedOut { .. } => None,
    }
}

#[test]
fn a_silent_peer_times_out_as_a_stall_and_shuts_the_socket() {
    let (mut socket, mut peer) = silent_pair(IDLE_200MS);
    let (socket, error) = fakes::within("a silent read to stall", EACH_WITHIN, move || {
        let error = socket.await_input(wait()).unwrap_err();
        (socket, error)
    });
    assert!(crate::timed_out(&error), "a silent peer times out: {error}");
    assert!(
        !matches!(error, ureq::Error::Timeout(_)),
        "an idle stall is Io, not Timeout: {error}"
    );
    assert_eq!(
        stalled_idle(&error),
        Some(IDLE_200MS),
        "the stall carries the idle bound"
    );
    // The socket stays alive, so only the stall's own shutdown ends the peer's read.
    let mut byte = [0u8; 1];
    let count = peer
        .read(&mut byte)
        .expect("the shutdown ends the peer's read as end of stream");
    assert_eq!(count, 0, "the peer sees end of stream after the stall");
    drop(socket);
}

#[test]
fn a_shorter_ureq_deadline_keeps_its_reason() {
    let (mut socket, _peer) = silent_pair(std::time::Duration::from_secs(10));
    let timeout = ureq_after(
        Duration::Exact(std::time::Duration::from_millis(100)),
        Timeout::Connect,
    );
    let error = fakes::within("a ureq-bound read to time out", EACH_WITHIN, move || {
        socket.await_input(timeout).unwrap_err()
    });
    assert!(
        matches!(error, ureq::Error::Timeout(Timeout::Connect)),
        "the shorter ureq bound keeps its reason: {error}"
    );
    assert!(crate::timed_out(&error), "a ureq timeout is a timeout");
}

/// `io_bound` uses ureq's bound only when strictly shorter: with `<` the
/// equal case is a stall, while `<=` would report a Timeout here.
#[test]
fn an_idle_equal_to_ureq_is_a_stall_not_a_timeout() {
    let (mut socket, _peer) = silent_pair(IDLE_200MS);
    let timeout = ureq_after(Duration::Exact(IDLE_200MS), Timeout::Global);
    let error = fakes::within("an equal-bound read to stall", EACH_WITHIN, move || {
        socket.await_input(timeout).unwrap_err()
    });
    assert!(
        stalled_idle(&error).is_some(),
        "equal bounds stall with the idle error, not Timeout: {error}"
    );
}

#[test]
fn an_expired_ureq_deadline_uses_its_one_second_substitute() {
    let (mut socket, _peer) = silent_pair(std::time::Duration::from_secs(10));
    let timeout = ureq_after(
        Duration::Exact(std::time::Duration::ZERO),
        Timeout::SendRequest,
    );
    let error = fakes::within("an expired deadline to time out", EACH_WITHIN, move || {
        socket.await_input(timeout).unwrap_err()
    });
    assert!(
        matches!(error, ureq::Error::Timeout(Timeout::SendRequest)),
        "an expired deadline keeps its reason after the 1 s substitute: {error}"
    );
}

// macOS sometimes ends a close with unread bytes in a plain end of stream
// instead of a reset, so only Linux gives this test a reset every run.
#[cfg(target_os = "linux")]
#[test]
fn a_reset_peer_is_not_a_stall() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut socket, peer) = fakes::within("the reset pair to connect", EACH_WITHIN, move || {
        let stream = TcpStream::connect(addr).unwrap();
        let (peer, _) = listener.accept().unwrap();
        (
            Socket {
                stream,
                buffers: LazyBuffers::new(1024, 1024),
                open: true,
                idle: std::time::Duration::from_secs(2),
            },
            peer,
        )
    });
    // Unread bytes in the peer's queue turn its close into a reset on Linux.
    let error = fakes::within("a reset read to fail", EACH_WITHIN, move || {
        socket.stream.write_all(&[7u8; 65536]).unwrap();
        drop(peer);
        socket.await_input(wait()).unwrap_err()
    });
    assert!(
        !crate::timed_out(&error),
        "a reset peer is not a stall: {error}"
    );
    assert!(
        matches!(
            &error,
            ureq::Error::Io(reset) if reset.kind() == io::ErrorKind::ConnectionReset
        ),
        "a reset peer reports the reset: {error}"
    );
}

#[test]
fn a_write_to_a_peer_that_never_reads_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (stream, peer) = fakes::within("the write pair to connect", EACH_WITHIN, move || {
        let stream = TcpStream::connect(addr).unwrap();
        let (peer, _) = listener.accept().unwrap();
        (stream, peer)
    });
    // The peer never reads; dropping it at the end releases its queue.
    let error = fakes::within("a stalled write to fail", EACH_WITHIN, move || {
        let mut socket = Socket {
            stream,
            buffers: LazyBuffers::new(1024, 16384),
            open: true,
            idle: IDLE_200MS,
        };
        let amount = 16384;
        for _ in 0..500 {
            match socket.transmit_output(amount, wait()) {
                Ok(()) => {}
                Err(failed) => return failed,
            }
        }
        panic!("500 full buffers left without stalling");
    });
    drop(peer);
    assert!(
        crate::timed_out(&error),
        "a write with no progress times out: {error}"
    );
    assert_eq!(
        stalled_idle(&error),
        Some(IDLE_200MS),
        "the write stall carries the idle bound"
    );
}

#[test]
fn a_held_server_fails_timed_out() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    server.hold();
    let url = server.url();
    let limits = short_limits();
    let error = fakes::within("a held GET to time out", EACH_WITHIN, move || {
        let agent = crate::agent(
            crate::config().proxy(None).build(),
            Arc::new(()),
            DefaultResolver::default(),
            limits,
        );
        agent.get(url).call().unwrap_err()
    });
    assert!(crate::timed_out(&error), "a held server times out: {error}");
}

#[test]
fn a_tls_handshake_that_never_answers_fails_timed_out() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        stream.set_read_timeout(Some(EACH_WITHIN)).unwrap_or(());
        let mut hello = [0u8; 1];
        // The ClientHello's first byte; then silence until the client
        // shuts the stalled socket down.
        match std::io::Read::read_exact(&mut stream, &mut hello) {
            Ok(()) | Err(_) => {}
        }
        let mut rest = [0u8; 1024];
        loop {
            match std::io::Read::read(&mut stream, &mut rest) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
    let limits = short_limits();
    let error = fakes::within(
        "a silent TLS handshake to time out",
        EACH_WITHIN,
        move || {
            let agent = crate::agent(
                crate::config().proxy(None).build(),
                Arc::new(()),
                DefaultResolver::default(),
                limits,
            );
            agent
                .get(format!("https://127.0.0.1:{port}/"))
                .call()
                .unwrap_err()
        },
    );
    assert!(
        crate::timed_out(&error),
        "a silent TLS handshake times out: {error}"
    );
}

/// Yields `Interrupted` `interruptions` times, then one byte, then a failure.
struct Interrupting {
    interruptions: usize,
}

impl Read for Interrupting {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.interruptions > 0 {
            self.interruptions -= 1;
            return Err(io::ErrorKind::Interrupted.into());
        }
        match buf.first_mut() {
            Some(byte) => {
                *byte = 7;
                Ok(1)
            }
            None => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }
}

#[test]
fn a_read_a_signal_interrupted_is_retried() {
    let mut buf = [0_u8; 4];
    let read = read_retrying(&mut Interrupting { interruptions: 3 }, &mut buf).unwrap();
    assert_eq!((read, buf[0]), (1, 7));
}

#[test]
fn a_read_failure_that_is_not_an_interruption_is_returned() {
    let error = read_retrying(&mut Interrupting { interruptions: 0 }, &mut [])
        .expect_err("the reader fails");
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

const CONNECT_15S: std::time::Duration = std::time::Duration::from_secs(15);

fn bound_for(after: Duration) -> (std::time::Duration, bool) {
    connect_bound(CONNECT_15S, ureq_after(after, Timeout::Connect))
}

#[test]
fn a_connect_waits_the_shorter_of_the_limit_and_ureqs_bound() {
    let secs = std::time::Duration::from_secs;
    // Just below, at and just above the limit; ureq's wins only when strictly shorter.
    assert_eq!(bound_for(Duration::Exact(secs(14))), (secs(14), true));
    assert_eq!(bound_for(Duration::Exact(secs(15))), (secs(15), false));
    assert_eq!(bound_for(Duration::Exact(secs(16))), (secs(15), false));
    assert_eq!(bound_for(Duration::NotHappening), (secs(15), false));
}

fn connect_failure(kind: io::ErrorKind, reason: Option<Timeout>) -> ureq::Error {
    let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    connect_error(io::Error::from(kind), addr, CONNECT_15S, reason)
}

#[test]
fn a_connect_that_reached_the_limit_is_a_connect_timeout() {
    let error = connect_failure(io::ErrorKind::TimedOut, None);
    let ureq::Error::Io(failed) = &error else {
        panic!("a connect limit is an Io error: {error}");
    };
    assert_eq!(failed.kind(), io::ErrorKind::TimedOut);
    let inner = failed.get_ref().and_then(|e| e.downcast_ref::<Error>());
    assert!(
        matches!(inner, Some(Error::ConnectTimedOut { addr, limit })
            if addr.port() == 9 && *limit == CONNECT_15S),
        "{error}"
    );
    assert!(crate::timed_out(&error));
}

#[test]
fn a_connect_that_reached_ureqs_bound_keeps_ureqs_reason() {
    let error = connect_failure(io::ErrorKind::TimedOut, Some(Timeout::Connect));
    assert!(
        matches!(error, ureq::Error::Timeout(Timeout::Connect)),
        "{error}"
    );
}

#[test]
fn a_refused_connect_is_not_a_timeout() {
    for reason in [None, Some(Timeout::Connect)] {
        let error = connect_failure(io::ErrorKind::ConnectionRefused, reason);
        assert!(
            matches!(&error, ureq::Error::Io(e) if e.kind() == io::ErrorKind::ConnectionRefused),
            "{error}"
        );
        assert!(!crate::timed_out(&error));
    }
}
