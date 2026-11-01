use super::*;
use fakes::Deadline;
use fakes::MUST_SUCCEED_WITHIN;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// How long a test-side wait lasts when it contains the fake server's own
/// socket timeouts: twice `MUST_SUCCEED_WITHIN`, so a stalled server step
/// expires first and its panic names the step (`docs/testing.md`, "Waits
/// and timeouts"). The socket timeouts stay `MUST_SUCCEED_WITHIN`: that
/// bound is a hang guard sized for full parallelism, and shortening it
/// manufactures flakes instead of diagnosing them.
const OUTER_WITHIN: Duration = Duration::from_secs(2 * MUST_SUCCEED_WITHIN.as_secs());

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
        stream.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
        stream.set_write_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
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
/// A blocking socket step runs on its own thread, and its outcome is
/// received with a deadline that names the awaited step.
#[track_caller]
fn await_step<T: Send + 'static>(what: &str, step: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, rx) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(step()).unwrap_or(());
    });
    Deadline::after(OUTER_WITHIN).recv_or_fail(&rx, what) // ruling: #3729
}

/// Reads one masked client frame: a stalled echo fails at the caller naming
/// the awaited step, and the opcode and mask assertions stay with the read.
#[track_caller]
fn client_frame(peer: &TcpStream, what: &str) -> (u8, Vec<u8>) {
    let mut stream = peer.try_clone().unwrap();
    let (header, bytes) = await_step(what, move || -> std::io::Result<([u8; 2], Vec<u8>)> {
        let mut header = [0; 2];
        stream.read_exact(&mut header)?;
        let len = match header[1] & 0x7f {
            126 => {
                let mut b = [0; 2];
                stream.read_exact(&mut b)?;
                usize::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0; 8];
                stream.read_exact(&mut b)?;
                usize::try_from(u64::from_be_bytes(b)).map_err(std::io::Error::other)?
            }
            n => usize::from(n),
        };
        let mut mask = [0; 4];
        stream.read_exact(&mut mask)?;
        let mut bytes = vec![0; len];
        stream.read_exact(&mut bytes)?;
        for (byte, mask) in bytes.iter_mut().zip(mask.iter().cycle()) {
            *byte ^= *mask;
        }
        Ok((header, bytes))
    })
    .unwrap_or_else(|error| panic!("{what}: {error}"));
    assert_eq!(header[0] & 0xf0, 0x80);
    assert_ne!(header[1] & 0x80, 0);
    (header[0] & 15, bytes)
}
fn exits(via: &mut Via) -> Receiver<&'static str> {
    let (tx, rx) = mpsc::channel();
    via.exits = Some(tx);
    rx
}
fn both(rx: &Receiver<&'static str>) {
    let mut names = [
        Deadline::after(OUTER_WITHIN).recv_or_fail(rx, "first websocket pump exit"), // ruling: #3729
        Deadline::after(OUTER_WITHIN).recv_or_fail(rx, "second websocket pump exit"), // ruling: #3729
    ];
    names.sort();
    assert_eq!(names, ["net-ws-in", "net-ws-out"]);
}
/// Writes caller bytes to the pair: a stalled write fails at the caller naming
/// the awaited step, and the stream returns for the next step.
#[track_caller]
fn caller_write(stream: UnixStream, bytes: Vec<u8>, what: &str) -> UnixStream {
    await_step(what, move || {
        let mut stream = stream;
        stream.write_all(&bytes).map(|_| stream)
    })
    .unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// Reads a stream to EOF: a missing EOF fails at the caller naming the
/// awaited step, and the stream must carry no bytes past its last line.
#[track_caller]
fn eof(stream: impl Read + Send + 'static, what: &str) {
    let bytes = await_step(what, move || {
        let mut stream = stream;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).map(|_| bytes)
    })
    .unwrap_or_else(|error| panic!("{what}: {error}"));
    assert!(
        bytes.is_empty(),
        "{what}: expected EOF, read {} bytes",
        bytes.len()
    );
}

#[test]
fn messages_lines_ping_pong_and_lossy_utf8_cross_the_pair() {
    let (endpoint, done) = server(|mut peer| {
        upgrade(&mut peer);
        peer.write_all(&server_frame(10, b"ignored")).unwrap();
        peer.write_all(&server_frame(1, b"hello")).unwrap();
        assert_eq!(
            client_frame(&peer, "server text echo"),
            (1, "a�".as_bytes().to_vec())
        );
        peer.write_all(&server_frame(9, b"ping")).unwrap();
        assert_eq!(client_frame(&peer, "server pong"), (10, b"ping".to_vec()));
        peer.write_all(&server_frame(8, &1001u16.to_be_bytes()))
            .unwrap();
        assert_eq!(
            client_frame(&peer, "server close echo"),
            (8, 1001u16.to_be_bytes().to_vec())
        );
        eof(peer, "server EOF after close");
    });
    let mut via = direct();
    let exit = exits(&mut via);
    let stream = connect_with(&endpoint, &via).unwrap();
    let read = BufReader::new(stream.try_clone().unwrap());
    let (read, line) = await_step(
        "client text line hello",
        move || -> std::io::Result<(BufReader<UnixStream>, String)> {
            let mut read = read;
            let mut line = String::new();
            read.read_line(&mut line).map(|_| (read, line))
        },
    )
    .unwrap_or_else(|error| panic!("client text line hello: {error}"));
    assert_eq!(line, "hello\n");
    let _stream = caller_write(stream, b"a\xff\n".to_vec(), "client write lossy line");
    eof(read, "client EOF after close");
    both(&exit);
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "server completed the bridge exchange"); // ruling: #3729
}

fn server_close(payload: Vec<u8>) {
    let (endpoint, done) = server(move |mut peer| {
        upgrade(&mut peer);
        peer.write_all(&server_frame(8, &payload)).unwrap();
        assert_eq!(client_frame(&peer, "server close echo"), (8, payload));
        eof(peer, "server EOF after echo");
    });
    let mut via = direct();
    let exit = exits(&mut via);
    let stream = connect_with(&endpoint, &via).unwrap();
    eof(stream, "client EOF after server close");
    both(&exit);
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "close echoed and server EOF"); // ruling: #3729
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
    let (endpoint, done) = server(|mut peer| {
        upgrade(&mut peer);
        assert_eq!(
            client_frame(&peer, "server normal close after caller drop"),
            (8, 1000u16.to_be_bytes().to_vec())
        );
        eof(peer, "server EOF after caller drop");
    });
    let mut via = direct();
    let exit = exits(&mut via);
    drop(connect_with(&endpoint, &via).unwrap());
    both(&exit);
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "caller EOF closed websocket"); // ruling: #3729
}

#[test]
fn eof_after_an_unterminated_client_line_still_sends_normal_close() {
    let (endpoint, done) = server(|mut peer| {
        upgrade(&mut peer);
        assert_eq!(
            client_frame(&peer, "server normal close after incomplete client line"),
            (8, 1000u16.to_be_bytes().to_vec())
        );
        eof(peer, "server EOF after normal close");
    });
    let mut via = direct();
    let exit = exits(&mut via);
    let mut stream = connect_with(&endpoint, &via).unwrap();
    stream = caller_write(
        stream,
        b"not a complete line".to_vec(),
        "client write incomplete line",
    );
    stream.shutdown(Shutdown::Write).unwrap();
    eof(stream, "client EOF after shutdown with incomplete line");
    both(&exit);
    Deadline::after(OUTER_WITHIN) // ruling: #3729
        .recv_or_fail(&done, "normal close after the incomplete client line");
}

#[test]
fn line_feed_in_a_server_message_closes_1007() {
    let (endpoint, done) = server(|mut peer| {
        upgrade(&mut peer);
        peer.write_all(&server_frame(1, b"not\na line")).unwrap();
        assert_eq!(
            client_frame(&peer, "server 1007 close after invalid line"),
            (8, 1007u16.to_be_bytes().to_vec())
        );
        eof(peer, "server EOF after 1007 close");
    });
    let mut via = direct();
    let exit = exits(&mut via);
    let stream = connect_with(&endpoint, &via).unwrap();
    eof(stream, "client EOF after 1007 close");
    both(&exit);
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "invalid message refused"); // ruling: #3729
}

#[test]
fn upgrade_reply_uses_the_idle_bound() {
    let (release, hold) = mpsc::channel::<()>();
    let (endpoint, done) = server(move |mut peer| {
        handshake::headers(&mut peer).unwrap();
        match hold.recv() {
            Ok(()) | Err(_) => {}
        }
    });
    let result = fakes::within(
        "upgrade idle bound",
        Deadline::after(OUTER_WITHIN), // ruling: #3729
        move || {
            let via = Via {
                limits: crate::Limits::new(Duration::from_secs(15), Duration::from_millis(100))
                    .unwrap(),
                ..direct()
            };
            connect_with(&endpoint, &via)
        },
    );
    assert!(matches!(result, Err(Error::Stalled { idle }) if idle == Duration::from_millis(100)));
    drop(release);
    fakes::Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "silent upgrade server released"); // ruling: #3729
}

#[test]
fn an_idle_websocket_keeps_its_read_open_after_the_upgrade() {
    let outer = Deadline::after(OUTER_WITHIN); // ruling: #3729
    fakes::within("idle websocket read", outer, || {
        let (release, hold) = mpsc::channel::<()>();
        let (endpoint, done) = server(move |mut peer| {
            upgrade(&mut peer);
            match hold.recv() {
                Ok(()) | Err(_) => {}
            }
            peer.write_all(&server_frame(1, b"after release")).unwrap();
        });
        let mut via = Via {
            limits: crate::Limits::new(Duration::from_secs(15), Duration::from_millis(100))
                .unwrap(),
            ..direct()
        };
        let exit = exits(&mut via);
        let stream = connect_with(&endpoint, &via).unwrap();
        let (line, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let result = BufReader::new(stream).read_line(&mut text);
            line.send((result, text)).unwrap();
        });
        assert!(matches!(
            fakes::Deadline::after(Duration::from_millis(400)).recv(&lines),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        let deadline = fakes::Deadline::after(OUTER_WITHIN); // ruling: #3729
        let (result, text) = deadline.recv_or_fail(&lines, "line after held idle interval");
        assert_eq!(result.unwrap(), 14);
        assert_eq!(text, "after release\n");
        both(&exit);
        deadline.recv_or_fail(&done, "held frame sent");
    });
}

#[test]
fn tls_receive_and_shutdown_do_not_wait_for_a_held_send() {
    let (send_frame, frame_wait) = mpsc::channel::<()>();
    let (close_peer, close_wait) = mpsc::channel::<()>();
    let (mut endpoint, done) = server(move |tcp| {
        let mut peer = rustls::StreamOwned::new(
            rustls::ServerConnection::new(tls::tests::server_config()).unwrap(),
            tcp,
        );
        upgrade(&mut peer);
        match frame_wait.recv() {
            Ok(()) | Err(_) => {}
        }
        peer.write_all(&server_frame(1, b"while held")).unwrap();
        match close_wait.recv() {
            Ok(()) | Err(_) => {}
        }
        peer.sock.shutdown(Shutdown::Both).unwrap();
    });
    endpoint.secure = true;
    let (held, is_held) = mpsc::channel();
    let (release, hold) = mpsc::channel::<()>();
    let mut via = Via {
        tls: tls::tests::trusted(),
        pause: Some(Arc::new(Pause {
            held,
            release: std::sync::Mutex::new(hold),
        })),
        ..direct()
    };
    let exit = exits(&mut via);
    let mut stream = connect_with(&endpoint, &via).unwrap();
    stream = caller_write(stream, b"held send\n".to_vec(), "client write held send");
    Deadline::after(OUTER_WITHIN).recv_or_fail(&is_held, "sender encrypted and released TLS state"); // ruling: #3729
    send_frame.send(()).unwrap();
    let (line, lines) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        BufReader::new(stream).read_line(&mut text).unwrap();
        line.send(text).unwrap();
    });
    assert_eq!(
        Deadline::after(OUTER_WITHIN) // ruling: #3729
            .recv_or_fail(&lines, "TLS plaintext while the sender is held"),
        "while held\n"
    );
    close_peer.send(()).unwrap();
    assert_eq!(
        Deadline::after(OUTER_WITHIN).recv_or_fail(&exit, "in-pump shutdown without the send lock"), // ruling: #3729
        "net-ws-in"
    );
    drop(release);
    assert_eq!(
        Deadline::after(OUTER_WITHIN).recv_or_fail(&exit, "held out-pump exits on the shut socket"), // ruling: #3729
        "net-ws-out"
    );
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "TLS server closed"); // ruling: #3729
}

#[test]
fn shutdown_with_unread_caller_bytes_still_delivers_eof() {
    let (close_peer, close_wait) = mpsc::channel::<()>();
    let (endpoint, done) = server(move |mut peer| {
        upgrade(&mut peer);
        match close_wait.recv() {
            Ok(()) | Err(_) => {}
        }
        peer.shutdown(Shutdown::Both).unwrap();
    });
    let (held, is_held) = mpsc::channel();
    let (release, hold) = mpsc::channel::<()>();
    let mut via = Via {
        pause: Some(Arc::new(Pause {
            held,
            release: std::sync::Mutex::new(hold),
        })),
        ..direct()
    };
    let exit = exits(&mut via);
    let mut stream = connect_with(&endpoint, &via).unwrap();
    stream = caller_write(
        stream,
        b"hold the sender\n".to_vec(),
        "client write hold the sender",
    );
    Deadline::after(OUTER_WITHIN).recv_or_fail(&is_held, "out-pump held before the TCP write"); // ruling: #3729
    // The out-pump cannot read these bytes: it is parked inside its send.
    stream = caller_write(
        stream,
        b"queued caller bytes\n".to_vec(),
        "client write queued caller bytes",
    );
    close_peer.send(()).unwrap();
    assert_eq!(
        Deadline::after(OUTER_WITHIN) // ruling: #3729
            .recv_or_fail(&exit, "in-pump shutdown while caller bytes are queued"),
        "net-ws-in"
    );
    drop(release);
    assert_eq!(
        Deadline::after(OUTER_WITHIN).recv_or_fail(&exit, "out-pump ends after shutdown"), // ruling: #3729
        "net-ws-out"
    );
    eof(stream, "caller EOF after shutdown with queued bytes");
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "server socket closed"); // ruling: #3729
}

#[test]
fn a_peer_that_never_reads_expires_the_write_and_ends_both_pumps() {
    let (release, hold) = mpsc::channel::<()>();
    let (endpoint, done) = server(move |mut peer| {
        upgrade(&mut peer);
        match hold.recv() {
            Ok(()) | Err(_) => {}
        }
    });
    let mut via = Via {
        limits: crate::Limits::new(Duration::from_secs(15), Duration::from_millis(200)).unwrap(),
        ..direct()
    };
    let exit = exits(&mut via);
    let mut stream = connect_with(&endpoint, &via).unwrap();
    stream.set_write_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let reader = stream.try_clone().unwrap();
    let (finished, eof_rx) = mpsc::channel();
    let (writer_done, writer_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = vec![b'x'; 65536];
        line.push(b'\n');
        while stream.write_all(&line).is_ok() {}
        writer_done.send(()).unwrap();
    });
    std::thread::spawn(move || {
        eof(reader, "client EOF after the websocket write stalls");
        finished.send(()).unwrap();
    });
    Deadline::after(OUTER_WITHIN).recv_or_fail(&eof_rx, "EOF after the websocket write stalls"); // ruling: #3729
    both(&exit);
    Deadline::after(OUTER_WITHIN).recv_or_fail(&writer_rx, "pair writer released by shutdown"); // ruling: #3729
    drop(release);
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "non-reading peer released"); // ruling: #3729
}

#[test]
fn an_unread_pair_is_bounded_and_completed_lines_stay_intact() {
    let (endpoint, done) = server(|mut peer| {
        upgrade(&mut peer);
        peer.write_all(&server_frame(1, b"first")).unwrap();
        let frame = server_frame(1, &vec![b'x'; 65536]);
        for _ in 0..1024 {
            if peer.write_all(&frame).is_err() {
                break;
            }
        }
    });
    let mut via = Via {
        limits: crate::Limits::new(Duration::from_secs(15), Duration::from_millis(200)).unwrap(),
        ..direct()
    };
    let exit = exits(&mut via);
    let stream = connect_with(&endpoint, &via).unwrap();
    both(&exit);
    let received = await_step("client stream EOF after pair backpressure", move || {
        let mut stream = stream;
        let mut received = Vec::new();
        stream.read_to_end(&mut received).map(|_| received)
    })
    .unwrap_or_else(|error| panic!("client stream EOF after pair backpressure: {error}"));
    // Only an LF completes a line. A failed stream may end in a cut-off tail.
    let mut lines = received
        .split_inclusive(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_suffix(b"\n"));
    assert_eq!(lines.next(), Some(b"first".as_slice()));
    for line in lines {
        assert_eq!(line.len(), 65536);
        assert!(line.iter().all(|byte| *byte == b'x'));
    }
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "flooding server stopped"); // ruling: #3729
}

#[test]
fn a_reset_peer_ends_the_pair_and_both_pumps() {
    let (endpoint, done) = server(|mut peer| {
        upgrade(&mut peer);
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [0x81]); /* unread frame bytes make close reset TCP */
    });
    let mut via = direct();
    let exit = exits(&mut via);
    let mut stream = connect_with(&endpoint, &via).unwrap();
    stream = caller_write(
        stream,
        b"unread payload\n".to_vec(),
        "client write unread payload",
    );
    eof(stream, "client EOF after reset with unread bytes");
    both(&exit);
    Deadline::after(OUTER_WITHIN).recv_or_fail(&done, "peer reset with unread bytes"); // ruling: #3729
}
