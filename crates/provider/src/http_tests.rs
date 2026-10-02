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

/// Records what it was asked to sign and adds one header.
struct Recorder(std::sync::Mutex<Vec<(String, String, Vec<u8>)>>);

impl contract::signing::Signer for Recorder {
    fn sign(
        &self,
        request: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        self.0.lock().unwrap().push((
            request.method.to_owned(),
            request.url.to_owned(),
            request.body.to_vec(),
        ));
        Ok(vec![("x-signature".to_owned(), "sig".to_owned())])
    }
}

/// Refuses to sign.
struct Refuses;

impl contract::signing::Signer for Refuses {
    fn sign(
        &self,
        _: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(contract::signing::Error::Failed("no key".to_owned()))
    }
}

#[test]
fn a_signer_is_asked_on_every_send_and_its_headers_are_sent() {
    let server = fakes::ProviderServer::start([
        fakes::Response::status(200, "{}"),
        fakes::Response::status(200, "{}"),
    ])
    .unwrap();
    let url = format!("{}/v1/responses", server.url());
    let signer = Recorder(std::sync::Mutex::default());
    let headers = [("x-client".to_owned(), "fiber".to_owned())];
    for _ in 0..2 {
        let sent = super::post_signed(
            &url,
            &headers,
            b"{\"a\":1}",
            Some(&signer),
            &std::sync::Arc::default(),
        );
        assert!(sent.is_ok());
    }
    let asked = signer.0.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "a retry signs again");
    assert_eq!(asked[0], ("POST".to_owned(), url, b"{\"a\":1}".to_vec()));
    for request in server.requests() {
        assert_eq!(request.header("x-signature"), Some("sig"));
        assert_eq!(request.header("x-client"), Some("fiber"));
        assert_eq!(request.body, b"{\"a\":1}");
    }
}

#[test]
fn a_request_that_cannot_be_signed_is_never_sent() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let sent = super::post_signed(
        &format!("{}/v1", server.url()),
        &[],
        b"",
        Some(&Refuses),
        &std::sync::Arc::default(),
    );
    let Err(err) = sent.map(|_| ()) else {
        panic!("signed anyway");
    };
    let crate::Error::Sign(why) = &err else {
        panic!("not a sign failure: {err:?}");
    };
    assert!(why.to_string().contains("no key"), "{why}");
    assert_eq!(err.code(), contract::ErrorCode::ConnectionFailed);
    assert_eq!(
        err.should_retry(),
        Some(false),
        "a sign failure is not retried"
    );
    assert!(server.requests().is_empty());
}
