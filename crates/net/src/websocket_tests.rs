use super::*;
use fakes::Deadline;
use fakes::MUST_SUCCEED_WITHIN;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

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
    fakes::within(
        "websocket line bridge",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
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
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            both(deadline, &exit);
            deadline.recv_or_fail(&done, "server completed the bridge exchange");
        },
    );
}

fn server_close(payload: Vec<u8>) {
    fakes::within(
        "server close",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
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
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            both(deadline, &exit);
            deadline.recv_or_fail(&done, "close echoed and server EOF");
        },
    );
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
    fakes::within("caller drop", Deadline::after(MUST_SUCCEED_WITHIN), || {
        let (endpoint, done) = server(|mut peer| {
            upgrade(&mut peer);
            assert_eq!(client_frame(&mut peer), (8, 1000u16.to_be_bytes().to_vec()));
            eof(&mut peer);
        });
        let mut via = direct();
        let exit = exits(&mut via);
        drop(connect_with(&endpoint, &via).unwrap());
        let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
        both(deadline, &exit);
        deadline.recv_or_fail(&done, "caller EOF closed websocket");
    });
}

#[test]
fn eof_after_an_unterminated_client_line_still_sends_normal_close() {
    fakes::within(
        "unterminated client line at EOF",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let (endpoint, done) = server(|mut peer| {
                upgrade(&mut peer);
                assert_eq!(client_frame(&mut peer), (8, 1000u16.to_be_bytes().to_vec()));
                eof(&mut peer);
            });
            let mut via = direct();
            let exit = exits(&mut via);
            let mut stream = connect_with(&endpoint, &via).unwrap();
            stream.write_all(b"not a complete line").unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            eof(&mut stream);
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            both(deadline, &exit);
            deadline.recv_or_fail(&done, "normal close after the incomplete client line");
        },
    );
}

#[test]
fn line_feed_in_a_server_message_closes_1007() {
    fakes::within(
        "invalid line message",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
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
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            both(deadline, &exit);
            deadline.recv_or_fail(&done, "invalid message refused");
        },
    );
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
        Deadline::after(MUST_SUCCEED_WITHIN),
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
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&done, "silent upgrade server released");
}

#[test]
fn an_idle_websocket_keeps_its_read_open_after_the_upgrade() {
    fakes::within(
        "idle websocket read",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
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
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            let (result, text) = deadline.recv_or_fail(&lines, "line after held idle interval");
            assert_eq!(result.unwrap(), 14);
            assert_eq!(text, "after release\n");
            both(deadline, &exit);
            deadline.recv_or_fail(&done, "held frame sent");
        },
    );
}

#[test]
fn tls_receive_and_shutdown_do_not_wait_for_a_held_send() {
    fakes::within(
        "TLS receive while send is held",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
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
            stream.write_all(b"held send\n").unwrap();
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            deadline.recv_or_fail(&is_held, "sender encrypted and released TLS state");
            send_frame.send(()).unwrap();
            let (line, lines) = mpsc::channel();
            std::thread::spawn(move || {
                let mut text = String::new();
                BufReader::new(stream).read_line(&mut text).unwrap();
                line.send(text).unwrap();
            });
            assert_eq!(
                deadline.recv_or_fail(&lines, "TLS plaintext while the sender is held"),
                "while held\n"
            );
            close_peer.send(()).unwrap();
            assert_eq!(
                deadline.recv_or_fail(&exit, "in-pump shutdown without the send lock"),
                "net-ws-in"
            );
            drop(release);
            assert_eq!(
                deadline.recv_or_fail(&exit, "held out-pump exits on the shut socket"),
                "net-ws-out"
            );
            deadline.recv_or_fail(&done, "TLS server closed");
        },
    );
}

#[test]
fn shutdown_with_unread_caller_bytes_still_delivers_eof() {
    fakes::within(
        "shutdown with unread caller bytes",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
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
            stream.write_all(b"hold the sender\n").unwrap();
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            deadline.recv_or_fail(&is_held, "out-pump held before the TCP write");
            // The out-pump cannot read these bytes: it is parked inside its send.
            stream.write_all(b"queued caller bytes\n").unwrap();
            close_peer.send(()).unwrap();
            assert_eq!(
                deadline.recv_or_fail(&exit, "in-pump shutdown while caller bytes are queued"),
                "net-ws-in"
            );
            drop(release);
            assert_eq!(
                deadline.recv_or_fail(&exit, "out-pump ends after shutdown"),
                "net-ws-out"
            );
            eof(&mut stream);
            deadline.recv_or_fail(&done, "server socket closed");
        },
    );
}

#[test]
fn a_peer_that_never_reads_expires_the_write_and_ends_both_pumps() {
    fakes::within(
        "websocket write backpressure",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let (release, hold) = mpsc::channel::<()>();
            let (endpoint, done) = server(move |mut peer| {
                upgrade(&mut peer);
                match hold.recv() {
                    Ok(()) | Err(_) => {}
                }
            });
            let mut via = Via {
                limits: crate::Limits::new(Duration::from_secs(15), Duration::from_millis(200))
                    .unwrap(),
                ..direct()
            };
            let exit = exits(&mut via);
            let mut stream = connect_with(&endpoint, &via).unwrap();
            stream.set_write_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
            let mut reader = stream.try_clone().unwrap();
            let (finished, eof_rx) = mpsc::channel();
            let (writer_done, writer_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut line = vec![b'x'; 65536];
                line.push(b'\n');
                while stream.write_all(&line).is_ok() {}
                writer_done.send(()).unwrap();
            });
            std::thread::spawn(move || {
                eof(&mut reader);
                finished.send(()).unwrap();
            });
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            deadline.recv_or_fail(&eof_rx, "EOF after the websocket write stalls");
            both(deadline, &exit);
            deadline.recv_or_fail(&writer_rx, "pair writer released by shutdown");
            drop(release);
            deadline.recv_or_fail(&done, "non-reading peer released");
        },
    );
}

#[test]
fn an_unread_pair_is_bounded_and_completed_lines_stay_intact() {
    fakes::within(
        "pair write backpressure",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
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
                limits: crate::Limits::new(Duration::from_secs(15), Duration::from_millis(200))
                    .unwrap(),
                ..direct()
            };
            let exit = exits(&mut via);
            let mut stream = connect_with(&endpoint, &via).unwrap();
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            both(deadline, &exit);
            let mut received = Vec::new();
            stream.read_to_end(&mut received).unwrap();
            // Only an LF completes a line. A failed stream may end in a cut-off tail.
            let mut lines = received
                .split_inclusive(|byte| *byte == b'\n')
                .filter_map(|line| line.strip_suffix(b"\n"));
            assert_eq!(lines.next(), Some(b"first".as_slice()));
            for line in lines {
                assert_eq!(line.len(), 65536);
                assert!(line.iter().all(|byte| *byte == b'x'));
            }
            deadline.recv_or_fail(&done, "flooding server stopped");
        },
    );
}

#[test]
fn a_reset_peer_ends_the_pair_and_both_pumps() {
    fakes::within(
        "reset websocket",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
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
            let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
            both(deadline, &exit);
            deadline.recv_or_fail(&done, "peer reset with unread bytes");
        },
    );
}
