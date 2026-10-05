use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;

/// How long a client read waits for the proxy's reply before the test fails.
const REPLY_WITHIN: Duration = Duration::from_secs(2);

/// How long the test waits for the proxy to record a CONNECT.
const CONNECT_WITHIN: Duration = Duration::from_secs(2);

/// How long the test waits for the proxy to see a tunnel close.
const CLOSE_WITHIN: Duration = Duration::from_secs(2);

/// How long a wait that must return at once may take: far under the 2s
/// deadlines above, far over an immediate return.
const QUICK: Duration = Duration::from_millis(500);

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
        .expect("the proxy's CONNECT reply");
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the CONNECT reply: {status:?}"
    );
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
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
        .expect("the proxy's CONNECT reply");
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the CONNECT reply: {status:?}"
    );
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    assert!(blank.trim().is_empty(), "the reply ends in a blank line");

    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);

    let mut client = reader.into_inner();
    client.write_all(b"ping").unwrap();
    client.flush().unwrap();
    let mut heard = [0; 4];
    client
        .read_exact(&mut heard)
        .expect("the echo through the tunnel");
    assert_eq!(&heard, b"ping");

    drop(client);
    assert!(
        proxy.await_closed(1, CLOSE_WITHIN),
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
        .expect("the proxy's 405 reply");
    assert!(
        status.starts_with("HTTP/1.1 405"),
        "a non-CONNECT reply: {status:?}"
    );
    let mut rest = Vec::new();
    assert!(
        reader.read_to_end(&mut rest).is_ok(),
        "the proxy closes after a 405"
    );
    assert!(proxy.connects().is_empty());
}

#[test]
fn await_connects_pins_its_boundary_and_deadline() {
    let proxy = ConnectProxy::start().unwrap();
    assert!(
        !proxy.await_connects(1, Duration::from_millis(100)),
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
        second.read_exact(&mut heard).unwrap();
        assert_eq!(&heard, b"ping");
    });
    assert!(
        proxy.await_connects(2, CONNECT_WITHIN),
        "both CONNECTs arrive"
    );
    assert_eq!(proxy.connects(), [first_target, second_target]);
    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "a count already exceeded is met"
    );
    assert!(
        !proxy.await_connects(3, Duration::from_millis(100)),
        "a count not yet met ends false"
    );
    // A count already met returns before the deadline: a `<=` for the `<`
    // in `await_connects` waits the whole deadline instead.
    thread::scope(|s| {
        let (done, finished) = mpsc::channel();
        let waiting = &proxy;
        s.spawn(move || {
            let met = waiting.await_connects(2, CONNECT_WITHIN);
            match done.send(met) {
                Ok(()) | Err(_) => {}
            }
        });
        assert_eq!(
            finished.recv_timeout(QUICK),
            Ok(true),
            "a met count returns before the deadline"
        );
    });
    first.write_all(b"ping").unwrap();
    let mut heard = [0; 4];
    first.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"ping");
    drop(first);
    arrived.join().unwrap();
    assert!(
        proxy.await_closed(2, CLOSE_WITHIN),
        "the proxy saw both tunnels close"
    );
    echo.join().unwrap();
    echo2.join().unwrap();
}

#[test]
fn await_closed_pins_its_deadline() {
    let proxy = ConnectProxy::start().unwrap();
    assert!(
        !proxy.await_closed(1, Duration::from_millis(100)),
        "nothing closed, so the wait ends false"
    );
    let (echo_addr, echo) = echo_once();
    let target = format!("{echo_addr}");
    let mut client = open_tunnel_on(proxy.port(), &target);
    client.write_all(b"ping").unwrap();
    let mut heard = [0; 4];
    client.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"ping");
    drop(client);
    assert!(
        proxy.await_closed(1, CLOSE_WITHIN),
        "the proxy saw the tunnel close"
    );
    // A count already met returns before the deadline: a `<=` for the `<`
    // in `await_closed` waits the whole deadline instead.
    thread::scope(|s| {
        let (done, finished) = mpsc::channel();
        let waiting = &proxy;
        s.spawn(move || {
            let closed = waiting.await_closed(1, CLOSE_WITHIN);
            match done.send(closed) {
                Ok(()) | Err(_) => {}
            }
        });
        assert_eq!(
            finished.recv_timeout(QUICK),
            Ok(true),
            "a met count returns before the deadline"
        );
    });
    echo.join().unwrap();
}

#[test]
fn a_dropped_proxy_refuses_connections() {
    let proxy = ConnectProxy::start().unwrap();
    let port = proxy.port();
    drop(proxy);
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the dropped proxy's port refuses connections"
    );
}

#[test]
fn a_connect_target_without_a_numeric_port_gets_400_and_records_nothing() {
    let proxy = ConnectProxy::start().unwrap();
    for target in ["not-a-target", "127.0.0.1:", "127.0.0.1:http"] {
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
            .expect("the proxy's 400 reply");
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "target {target:?}: {status:?}"
        );
        let mut rest = Vec::new();
        assert!(
            reader.read_to_end(&mut rest).is_ok(),
            "the proxy closes after a 400"
        );
    }
    assert!(proxy.connects().is_empty());
}
