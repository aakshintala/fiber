use super::*;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);
fn direct() -> Via {
    Via {
        proxy: None,
        ..Via::from_env()
    }
}
fn server(f: impl FnOnce(TcpStream) + Send + 'static) -> (Endpoint, Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = Endpoint {
        secure: false,
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        target: "/hub".into(),
    };
    let (done, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.set_write_timeout(Some(WAIT)).unwrap();
        f(stream);
        done.send(()).unwrap();
    });
    (endpoint, rx)
}
fn upgrade(stream: &mut (impl Read + Write)) {
    let (head, _) = handshake::headers(stream).unwrap();
    let key = head
        .lines()
        .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
        .unwrap();
    let accept = handshake::accept(key);
    stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).unwrap();
}
fn server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        0..=125 => frame.push(u8::try_from(payload.len()).unwrap()),
        126..=65535 => {
            frame.push(126);
            frame.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        }
        _ => {
            frame.push(127);
            frame.extend_from_slice(&u64::try_from(payload.len()).unwrap().to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
}
fn client_frame(stream: &mut impl Read) -> (u8, Vec<u8>) {
    let mut header = [0; 2];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(header[0] & 0xf0, 0x80);
    assert_ne!(header[1] & 0x80, 0);
    let len = match header[1] & 0x7f {
        126 => {
            let mut b = [0; 2];
            stream.read_exact(&mut b).unwrap();
            usize::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0; 8];
            stream.read_exact(&mut b).unwrap();
            usize::try_from(u64::from_be_bytes(b)).unwrap()
        }
        n => usize::from(n),
    };
    let mut mask = [0; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).unwrap();
    for (byte, mask) in bytes.iter_mut().zip(mask.iter().cycle()) {
        *byte ^= *mask;
    }
    (header[0] & 15, bytes)
}
fn exits(via: &mut Via) -> Receiver<&'static str> {
    let (tx, rx) = mpsc::channel();
    via.exits = Some(tx);
    rx
}
fn both(deadline: fakes::Deadline, rx: &Receiver<&'static str>) {
    let mut names = [
        deadline.recv_or_fail(rx, "first websocket pump exit"),
        deadline.recv_or_fail(rx, "second websocket pump exit"),
    ];
    names.sort();
    assert_eq!(names, ["net-ws-in", "net-ws-out"]);
}
fn eof(stream: &mut impl Read) {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty());
}

#[test]
fn messages_lines_ping_pong_and_lossy_utf8_cross_the_pair() {
    fakes::within("websocket line bridge", WAIT, || {
        let (endpoint, done) = server(|mut peer| {
            upgrade(&mut peer);
            peer.write_all(&server_frame(10, b"ignored")).unwrap();
            peer.write_all(&server_frame(1, b"hello")).unwrap();
            assert_eq!(client_frame(&mut peer), (1, "a�".as_bytes().to_vec()));
            peer.write_all(&server_frame(9, b"ping")).unwrap();
            assert_eq!(client_frame(&mut peer), (10, b"ping".to_vec()));
            peer.write_all(&server_frame(8, &1001u16.to_be_bytes()))
                .unwrap();
            assert_eq!(client_frame(&mut peer), (8, 1001u16.to_be_bytes().to_vec()));
            eof(&mut peer);
        });
        let mut via = direct();
        let exit = exits(&mut via);
        let mut stream = connect_with(&endpoint, &via).unwrap();
        let mut read = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        read.read_line(&mut line).unwrap();
        assert_eq!(line, "hello\n");
        stream.write_all(b"a\xff\n").unwrap();
        eof(&mut read);
        let deadline = fakes::Deadline::after(WAIT);
        both(deadline, &exit);
        deadline.recv_or_fail(&done, "server completed the bridge exchange");
    });
}

fn server_close(payload: Vec<u8>) {
    fakes::within("server close", WAIT, move || {
        let (endpoint, done) = server(move |mut peer| {
            upgrade(&mut peer);
            peer.write_all(&server_frame(8, &payload)).unwrap();
            assert_eq!(client_frame(&mut peer), (8, payload));
            eof(&mut peer);
        });
        let mut via = direct();
        let exit = exits(&mut via);
        let mut stream = connect_with(&endpoint, &via).unwrap();
        eof(&mut stream);
        let deadline = fakes::Deadline::after(WAIT);
        both(deadline, &exit);
        deadline.recv_or_fail(&done, "close echoed and server EOF");
    });
}
#[test]
fn server_close_without_a_code_is_echoed() {
    server_close(Vec::new());
}
#[test]
fn server_close_with_a_code_is_echoed() {
    server_close(1001u16.to_be_bytes().to_vec());
}

#[test]
fn dropping_the_pair_sends_normal_close_and_ends_both_pumps() {
    fakes::within("caller drop", WAIT, || {
        let (endpoint, done) = server(|mut peer| {
            upgrade(&mut peer);
            assert_eq!(client_frame(&mut peer), (8, 1000u16.to_be_bytes().to_vec()));
            eof(&mut peer);
        });
        let mut via = direct();
        let exit = exits(&mut via);
        drop(connect_with(&endpoint, &via).unwrap());
        let deadline = fakes::Deadline::after(WAIT);
        both(deadline, &exit);
        deadline.recv_or_fail(&done, "caller EOF closed websocket");
    });
}

#[test]
fn line_feed_in_a_server_message_closes_1007() {
    fakes::within("invalid line message", WAIT, || {
        let (endpoint, done) = server(|mut peer| {
            upgrade(&mut peer);
            peer.write_all(&server_frame(1, b"not\na line")).unwrap();
            assert_eq!(client_frame(&mut peer), (8, 1007u16.to_be_bytes().to_vec()));
            eof(&mut peer);
        });
        let mut via = direct();
        let exit = exits(&mut via);
        let mut stream = connect_with(&endpoint, &via).unwrap();
        eof(&mut stream);
        let deadline = fakes::Deadline::after(WAIT);
        both(deadline, &exit);
        deadline.recv_or_fail(&done, "invalid message refused");
    });
}

#[test]
fn a_reset_peer_ends_the_pair_and_both_pumps() {
    fakes::within("reset websocket", WAIT, || {
        let (endpoint, done) = server(|mut peer| {
            upgrade(&mut peer);
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [0x81]); /* unread frame bytes make close reset TCP */
        });
        let mut via = direct();
        let exit = exits(&mut via);
        let mut stream = connect_with(&endpoint, &via).unwrap();
        stream.write_all(b"unread payload\n").unwrap();
        eof(&mut stream);
        let deadline = fakes::Deadline::after(WAIT);
        both(deadline, &exit);
        deadline.recv_or_fail(&done, "peer reset with unread bytes");
    });
}
