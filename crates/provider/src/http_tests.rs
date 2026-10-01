use std::io::Write;
use std::net::{TcpListener, TcpStream};

use ureq::Timeout;
use ureq::unversioned::transport::time::Duration;
use ureq::unversioned::transport::{LazyBuffers, NextTimeout, Transport};

use super::Socket;

fn wait() -> NextTimeout {
    NextTimeout {
        after: Duration::NotHappening,
        reason: Timeout::Global,
    }
}

#[test]
fn a_socket_is_open_until_a_read_finds_the_peer_closed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    let mut socket = Socket {
        stream,
        buffers: LazyBuffers::new(1024, 1024),
        open: true,
    };
    assert!(socket.is_open());

    peer.write_all(b"x").unwrap();
    assert!(socket.await_input(wait()).unwrap());
    assert!(socket.is_open(), "a read that got bytes leaves it open");

    drop(peer);
    assert!(!socket.await_input(wait()).unwrap());
    assert!(!socket.is_open(), "a read that found the peer closed");
}
