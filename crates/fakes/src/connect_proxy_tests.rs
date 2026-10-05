use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;

/// How long a client read waits for the proxy's reply before the test fails.
const REPLY_WITHIN: Duration = Duration::from_secs(2);

/// How long the test waits for the proxy to record a CONNECT.
const CONNECT_WITHIN: Duration = Duration::from_secs(2);

/// How long the test waits for the proxy to see a tunnel close.
const CLOSE_WITHIN: Duration = Duration::from_secs(2);

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
            let _ = stream.read_to_end(&mut rest);
        })
        .unwrap();
    (addr, echo)
}

fn connect(proxy: &ConnectProxy) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
    stream.set_read_timeout(Some(REPLY_WITHIN)).unwrap();
    stream
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
    client.read_exact(&mut heard).expect("the echo through the tunnel");
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
    reader.read_line(&mut status).expect("the proxy's 405 reply");
    assert!(
        status.starts_with("HTTP/1.1 405"),
        "a non-CONNECT reply: {status:?}"
    );
    let mut head = String::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("the proxy's 405 head");
        head.push_str(&header);
        if header.trim().is_empty() {
            break;
        }
    }
    assert!(head.contains("content-length: 0"), "an empty 405 body: {head:?}");
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "the proxy closes after a 405");
    assert!(proxy.connects().is_empty());
}
