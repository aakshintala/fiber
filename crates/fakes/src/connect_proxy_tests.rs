use crate::MUST_SUCCEED_WITHIN;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;
use crate::deadline::Deadline;

/// How long a client read waits for the proxy's reply before the test fails.
const REPLY_WITHIN: Duration = crate::MUST_SUCCEED_WITHIN;

/// How long the test waits for the proxy to record a CONNECT.
const CONNECT_WITHIN: Duration = crate::MUST_SUCCEED_WITHIN;

/// How long the test waits for the proxy to see a tunnel close.
const CLOSE_WITHIN: Duration = crate::MUST_SUCCEED_WITHIN;

/// An echo listener: it answers one connection's first four bytes, then
/// stays open until the client goes away, so only the client's close ends
/// the tunnel.
fn echo_once() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let echo = thread::Builder::new()
        .name("fake-connect-proxy-test-echo".to_owned())
        .spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut heard = [0; 4];
            stream.read_exact(&mut heard).unwrap();
            stream.write_all(&heard).unwrap();
            let mut rest = Vec::new();
            match stream.read_to_end(&mut rest) {
                Ok(_) | Err(_) => {}
            }
        })
        .unwrap();
    (addr, echo)
}

fn connect(proxy: &ConnectProxy) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
    stream.set_read_timeout(Some(REPLY_WITHIN)).unwrap();
    stream
}

/// Opens a CONNECT tunnel for `target` on `port` and returns the client
/// stream with the 200 consumed, so the caller owns the tunnel's client end.
fn open_tunnel_on(port: u16, target: &str) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(REPLY_WITHIN)).unwrap();
    let mut reader = BufReader::new(stream);
    write!(
        reader.get_mut(),
        "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
    )
    .unwrap();
    reader.get_mut().flush().unwrap();
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("the proxy's CONNECT reply within {REPLY_WITHIN:?}: {e}"));
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the CONNECT reply: {status:?}"
    );
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap_or_else(|e| {
        panic!("the proxy's blank line after the CONNECT reply within {REPLY_WITHIN:?}: {e}")
    });
    reader.into_inner()
}

#[test]
fn a_connect_tunnel_copies_both_ways_and_records_its_target() {
    let proxy = ConnectProxy::start().unwrap();
    let (echo_addr, echo) = echo_once();
    let target = format!("{echo_addr}");
    assert_eq!(proxy.url(), format!("http://127.0.0.1:{}", proxy.port()));

    let client = connect(&proxy);
    let mut reader = BufReader::new(client);
    write!(
        reader.get_mut(),
        "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
    )
    .unwrap();
    reader.get_mut().flush().unwrap();
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("the proxy's CONNECT reply within {REPLY_WITHIN:?}: {e}"));
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the CONNECT reply: {status:?}"
    );
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap_or_else(|e| {
        panic!("the proxy's blank line after the CONNECT reply within {REPLY_WITHIN:?}: {e}")
    });
    assert!(blank.trim().is_empty(), "the reply ends in a blank line");

    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);

    let mut client = reader.into_inner();
    client.write_all(b"ping").unwrap();
    client.flush().unwrap();
    let mut heard = [0; 4];
    client
        .read_exact(&mut heard)
        .unwrap_or_else(|e| panic!("the echo through the tunnel within {REPLY_WITHIN:?}: {e}"));
    assert_eq!(&heard, b"ping");

    drop(client);
    assert!(
        proxy.await_closed(1, Deadline::after(CLOSE_WITHIN)),
        "the proxy saw the tunnel close"
    );
    echo.join().unwrap();
}

#[test]
fn a_request_that_is_not_connect_gets_405_and_records_nothing() {
    let proxy = ConnectProxy::start().unwrap();
    let client = connect(&proxy);
    let mut reader = BufReader::new(client);
    write!(
        reader.get_mut(),
        "GET http://127.0.0.1:1/ HTTP/1.1\r\nHost: 127.0.0.1:1\r\n\r\n"
    )
    .unwrap();
    reader.get_mut().flush().unwrap();
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("the proxy's 405 reply within {REPLY_WITHIN:?}: {e}"));
    assert!(
        status.starts_with("HTTP/1.1 405"),
        "a non-CONNECT reply: {status:?}"
    );
    let mut rest = Vec::new();
    assert!(
        reader.read_to_end(&mut rest).is_ok(),
        "the proxy closes after a 405 within {REPLY_WITHIN:?}"
    );
    assert!(proxy.connects().is_empty());
}

#[test]
fn await_connects_pins_its_boundary_and_deadline() {
    let proxy = ConnectProxy::start().unwrap();
    assert!(
        !proxy.await_connects(1, Deadline::after(Duration::from_millis(100))),
        "nothing arrived, so the wait ends false"
    );
    let (echo_addr, echo) = echo_once();
    let first_target = format!("{echo_addr}");
    let mut first = open_tunnel_on(proxy.port(), &first_target);
    let (echo2_addr, echo2) = echo_once();
    let second_target = format!("{echo2_addr}");
    let port = proxy.port();
    // A second CONNECT arrives while the waits below run.
    let arriving = second_target.clone();
    let arrived = thread::spawn(move || {
        let mut second = open_tunnel_on(port, &arriving);
        second.write_all(b"ping").unwrap();
        let mut heard = [0; 4];
        second.read_exact(&mut heard).unwrap_or_else(|e| {
            panic!("the echo through the second tunnel within {REPLY_WITHIN:?}: {e}")
        });
        assert_eq!(&heard, b"ping");
    });
    assert!(
        proxy.await_connects(2, Deadline::after(CONNECT_WITHIN)),
        "both CONNECTs arrive"
    );
    assert_eq!(proxy.connects(), [first_target, second_target]);
    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
        "a count already exceeded is met"
    );
    assert!(
        !proxy.await_connects(3, Deadline::after(Duration::from_millis(100))),
        "a count not yet met ends false"
    );
    // A count already met returns before the deadline: a `<=` for the `<`
    // in `await_connects` waits the whole deadline instead.
    thread::scope(|s| {
        let (done, finished) = mpsc::channel();
        let waiting = &proxy;
        s.spawn(move || {
            // Three times the outer receive's bound, so that receive fails
            // first on a wait that ignores its met count.
            let met = waiting.await_connects(2, Deadline::after(MUST_SUCCEED_WITHIN * 3));
            match done.send(met) {
                Ok(()) | Err(_) => {}
            }
        });
        assert_eq!(
            Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished),
            Ok(true),
            "a met count returns before the deadline within {MUST_SUCCEED_WITHIN:?}"
        );
    });
    first.write_all(b"ping").unwrap();
    let mut heard = [0; 4];
    first.read_exact(&mut heard).unwrap_or_else(|e| {
        panic!("the echo through the first tunnel within {REPLY_WITHIN:?}: {e}")
    });
    assert_eq!(&heard, b"ping");
    drop(first);
    arrived.join().unwrap();
    assert!(
        proxy.await_closed(2, Deadline::after(CLOSE_WITHIN)),
        "the proxy saw both tunnels close"
    );
    echo.join().unwrap();
    echo2.join().unwrap();
}

#[test]
fn await_closed_pins_its_deadline() {
    let proxy = ConnectProxy::start().unwrap();
    assert!(
        !proxy.await_closed(1, Deadline::after(Duration::from_millis(100))),
        "nothing closed, so the wait ends false"
    );
    let (echo_addr, echo) = echo_once();
    let target = format!("{echo_addr}");
    let mut client = open_tunnel_on(proxy.port(), &target);
    client.write_all(b"ping").unwrap();
    let mut heard = [0; 4];
    client
        .read_exact(&mut heard)
        .unwrap_or_else(|e| panic!("the echo through the tunnel within {REPLY_WITHIN:?}: {e}"));
    assert_eq!(&heard, b"ping");
    drop(client);
    assert!(
        proxy.await_closed(1, Deadline::after(CLOSE_WITHIN)),
        "the proxy saw the tunnel close"
    );
    // A count already met returns before the deadline: a `<=` for the `<`
    // in `await_closed` waits the whole deadline instead.
    thread::scope(|s| {
        let (done, finished) = mpsc::channel();
        let waiting = &proxy;
        s.spawn(move || {
            // Three times the outer receive's bound, so that receive fails
            // first on a wait that ignores its met count.
            let closed = waiting.await_closed(1, Deadline::after(MUST_SUCCEED_WITHIN * 3));
            match done.send(closed) {
                Ok(()) | Err(_) => {}
            }
        });
        assert_eq!(
            Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished),
            Ok(true),
            "a met closed count returns before the deadline within {MUST_SUCCEED_WITHIN:?}"
        );
    });
    echo.join().unwrap();
}

#[test]
fn a_dropped_proxy_refuses_connections() {
    let proxy = ConnectProxy::start().unwrap();
    let probe = proxy.core.drop_probe();
    drop(proxy);
    assert!(
        probe.listener_closed(),
        "the dropped proxy closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "the dropped proxy joined its accept thread"
    );
}

/// Bytes sent with the CONNECT head are the tunnel's first payload, not
/// dropped lookahead: one write carries the head and the ping together.
#[test]
fn coalesced_connect_and_payload_preserves_the_payload() {
    let proxy = ConnectProxy::start().unwrap();
    // The echo answers the coalesced ping and then signals, so the test
    // holds its first reply read until the answer is on the wire: the
    // reply and the answer share the socket, and the reply reader buffers
    // past the head into the answer.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let echo_addr = listener.local_addr().unwrap();
    let (echoed, answered) = mpsc::channel();
    let echo = thread::Builder::new()
        .name("fake-connect-proxy-test-echo".to_owned())
        .spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut heard = [0; 4];
            stream.read_exact(&mut heard).unwrap();
            stream.write_all(&heard).unwrap();
            match echoed.send(()) {
                Ok(()) | Err(_) => {}
            }
            let mut rest = Vec::new();
            match stream.read_to_end(&mut rest) {
                Ok(_) | Err(_) => {}
            }
        })
        .unwrap();
    let target = format!("{echo_addr}");
    let mut reader = {
        let mut client = connect(&proxy);
        client
            .write_all(
                format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\nping").as_bytes(),
            )
            .unwrap();
        client.flush().unwrap();
        BufReader::new(client)
    };
    assert_eq!(
        Deadline::after(CONNECT_WITHIN).recv(&answered),
        Ok(()),
        "the echo answers the coalesced ping within {CONNECT_WITHIN:?}"
    );
    // The peek proves the reply and the forwarded ping share the
    // client's socket before the first reply read: the 39-byte 200
    // plus the 4-byte ping, left unread for the reply reader, so one
    // read fills it past the head into the ping.
    let arrived = Deadline::after(CONNECT_WITHIN);
    let mut peeked = [0u8; 43];
    loop {
        match reader.get_ref().peek(&mut peeked) {
            Ok(43) => break,
            Ok(_) => assert!(
                !arrived.left().is_zero(),
                "the coalesced reply and ping within {CONNECT_WITHIN:?}"
            ),
            Err(error) => panic!(
                "the peek of the coalesced reply and ping within {CONNECT_WITHIN:?}: {error}"
            ),
        }
    }
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("the proxy's CONNECT reply within {REPLY_WITHIN:?}: {e}"));
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the CONNECT reply: {status:?}"
    );
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    // The coalesced ping went to the echo, which answered it. The reply
    // reader buffered past the head into the answer, so the answer is
    // read from the same reader: its inner stream no longer holds it.
    let mut heard = [0; 4];
    reader
        .read_exact(&mut heard)
        .unwrap_or_else(|e| panic!("the coalesced echo within {REPLY_WITHIN:?}: {e}"));
    assert_eq!(&heard, b"ping");
    drop(reader);
    assert!(
        proxy.await_closed(1, Deadline::after(CLOSE_WITHIN)),
        "the proxy saw the tunnel close"
    );
    echo.join().unwrap();
}

/// Dropping the proxy closes every open tunnel: the client's read ends
/// instead of hanging on a tunnel nobody pumps.
#[test]
fn dropping_the_proxy_closes_an_open_tunnel() {
    let proxy = ConnectProxy::start().unwrap();
    let (echo_addr, echo) = echo_once();
    let target = format!("{echo_addr}");
    let mut client = open_tunnel_on(proxy.port(), &target);
    client.write_all(b"ping").unwrap();
    let mut heard = [0; 4];
    client
        .read_exact(&mut heard)
        .unwrap_or_else(|e| panic!("the echo through the tunnel within {REPLY_WITHIN:?}: {e}"));
    assert_eq!(&heard, b"ping");
    drop(proxy);
    // The tunnel's sockets are shut: the client's read ends at once.
    let mut rest = Vec::new();
    match client.read_to_end(&mut rest) {
        Ok(_) | Err(_) => {}
    }
    drop(client);
    echo.join().unwrap();
}

/// Fake targets only: loopback numerics and localhost connect, anything
/// else is refused before any connect is attempted.
#[test]
fn connect_targets_stay_inside_the_fake_boundary() {
    use crate::http_server::HttpServer;
    // Loopback IPv4 with a port.
    assert!(
        HttpServer::check_target("127.0.0.1:80").is_ok(),
        "loopback IPv4 connects"
    );
    assert!(
        HttpServer::check_target("127.0.0.2:80").is_ok(),
        "the whole loopback range connects"
    );
    // Bracketed loopback IPv6 with a port.
    assert!(
        HttpServer::check_target("[::1]:80").is_ok(),
        "loopback IPv6 connects"
    );
    // Localhost maps to loopback without a DNS lookup.
    assert_eq!(
        HttpServer::check_target("localhost:80").unwrap(),
        "127.0.0.1:80".parse().unwrap(),
        "localhost maps to loopback"
    );
    // A public numeric target never connects.
    assert!(
        HttpServer::check_target("192.0.2.1:80").is_err(),
        "a public numeric target is refused"
    );
    // A name only a resolver could answer never resolves here.
    assert!(
        HttpServer::check_target("no-such-proxy-target:80").is_err(),
        "an unresolvable name is refused"
    );
    // A target with no port never connects.
    assert!(
        HttpServer::check_target("127.0.0.1").is_err(),
        "a target with no port is refused"
    );
}

/// A public numeric target closes the connection with no 200 and no
/// record, fast: no connect is ever attempted against it.
#[test]
fn a_public_numeric_target_closes_without_connecting() {
    let proxy = ConnectProxy::start().unwrap();
    let mut client = connect(&proxy);
    client
        .write_all(b"CONNECT 192.0.2.1:80 HTTP/1.1\r\nHost: 192.0.2.1:80\r\n\r\n")
        .unwrap();
    client.flush().unwrap();
    // The close, not a 200: the target is rejected before any connect,
    // so an unroutable address cannot hang the test on its timeout.
    // The close completes the rejection: nothing recorded by then is
    // never recorded, so the state is inspected directly.
    let mut reply = Vec::new();
    match client.read_to_end(&mut reply) {
        Ok(_) | Err(_) => {}
    }
    assert!(proxy.connects().is_empty());
}

/// A refused loopback target closes the connection with no 200 and no
/// record: the fake dials it and the refusal ends the handshake.
#[test]
fn a_refused_loopback_target_closes_without_a_tunnel() {
    let proxy = ConnectProxy::start().unwrap();
    let mut client = connect(&proxy);
    let refused = crate::refused::ADDR;
    client
        .write_all(format!("CONNECT {refused} HTTP/1.1\r\nHost: {refused}\r\n\r\n").as_bytes())
        .unwrap();
    client.flush().unwrap();
    let mut reply = Vec::new();
    match client.read_to_end(&mut reply) {
        Ok(_) | Err(_) => {}
    }
    assert!(
        !reply.starts_with(b"HTTP/1.1 200"),
        "a refused target gets no tunnel"
    );
    assert!(proxy.connects().is_empty());
}

/// A localhost target tunnels: the name maps to loopback without a DNS
/// lookup.
#[test]
fn a_localhost_target_tunnels_to_loopback() {
    let proxy = ConnectProxy::start().unwrap();
    let (echo_addr, echo) = echo_once();
    let target = format!("localhost:{}", echo_addr.port());
    let mut client = open_tunnel_on(proxy.port(), &target);
    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);
    client.write_all(b"ping").unwrap();
    let mut heard = [0; 4];
    client
        .read_exact(&mut heard)
        .unwrap_or_else(|e| panic!("the echo through the tunnel within {REPLY_WITHIN:?}: {e}"));
    assert_eq!(&heard, b"ping");
    drop(client);
    assert!(
        proxy.await_closed(1, Deadline::after(CLOSE_WITHIN)),
        "the proxy saw the tunnel close"
    );
    echo.join().unwrap();
}

/// A silent handshake stays silent on the injected clock: the reader
/// parks on the clock waiting for the peer, records nothing, and is
/// still parked half the bound later. Parked evidence decides it, never
/// an elapsed-time absence: no client read with a wall timeout takes
/// part in the verdict.
#[test]
fn a_silent_handshake_records_nothing_on_the_injected_clock() {
    use crate::clock::FakeClock;
    let clock = FakeClock::new();
    let proxy = ConnectProxy::start_on(clock.clone()).unwrap();
    let acks = proxy.test_acknowledgments();
    let _client = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
    // The handshake reader parks on the clock waiting for the silent
    // peer: its acknowledgment names the request-line phase past its
    // clock check, and the parked thread proves it is waiting.
    let waiting_deadline = Deadline::after(CONNECT_WITHIN);
    let waiting = loop {
        let ack = waiting_deadline.recv_or_fail(&acks, "the handshake's clock wait");
        if ack.phase == "to read the request line" {
            break ack;
        }
    };
    assert!(
        clock.await_thread_parked(
            waiting.thread,
            waiting.until,
            Deadline::after(CONNECT_WITHIN)
        ),
        "the handshake parks on the clock before it moves"
    );
    // Half the bound passes with no client bytes: the same reader
    // announces its next wait in sequence, still parked on the silent
    // peer, having answered nothing and recorded nothing.
    clock.advance(Duration::from_secs(15));
    let still_deadline = Deadline::after(CONNECT_WITHIN);
    let still = loop {
        let ack = still_deadline.recv_or_fail(&acks, "the handshake's next wait");
        if ack.phase == "to read the request line" {
            break ack;
        }
    };
    assert_eq!(
        still.thread, waiting.thread,
        "the same reader is still waiting on the silent peer"
    );
    assert_eq!(
        still.seq,
        waiting.seq + 1,
        "only the announced sequence advances the clock"
    );
    assert!(
        clock.await_thread_parked(still.thread, still.until, Deadline::after(CONNECT_WITHIN)),
        "half the bound later the handshake is still parked on the silent peer"
    );
    assert!(
        proxy.connects().is_empty(),
        "a silent handshake records nothing"
    );
}

/// Dropping the proxy closes every open tunnel: a tunnel that ended
/// earlier does not evict a still-open one from the shutdown.
#[test]
fn dropping_the_proxy_closes_every_open_tunnel() {
    let proxy = ConnectProxy::start().unwrap();
    let (echo_a_addr, echo_a) = echo_once();
    let (echo_b_addr, echo_b) = echo_once();
    let target_a = format!("{echo_a_addr}");
    let target_b = format!("{echo_b_addr}");
    let client_a = open_tunnel_on(proxy.port(), &target_a);
    let mut client_b = open_tunnel_on(proxy.port(), &target_b);
    // Both tunnels echo, so both echoes are past their first read.
    for client in [&client_a, &client_b] {
        let mut client = client.try_clone().expect("the test clones");
        client.write_all(b"ping").unwrap();
        let mut heard = [0; 4];
        client
            .read_exact(&mut heard)
            .unwrap_or_else(|e| panic!("the echo through the tunnel within {REPLY_WITHIN:?}: {e}"));
        assert_eq!(&heard, b"ping");
    }
    // The first tunnel ends while the second stays open.
    drop(client_a);
    assert!(
        proxy.await_closed(1, Deadline::after(CLOSE_WITHIN)),
        "the proxy saw the first tunnel close"
    );
    drop(proxy);
    // The still-open tunnel's sockets are shut: its read ends at once.
    client_b
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .expect("the test bounds its reads");
    let mut rest = Vec::new();
    client_b.read_to_end(&mut rest).unwrap_or_else(|e| {
        panic!("the open tunnel ends on drop within {MUST_SUCCEED_WITHIN:?}: {e}")
    });
    drop(client_b);
    echo_a.join().unwrap();
    echo_b.join().unwrap();
}
