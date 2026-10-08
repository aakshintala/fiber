use std::io::ErrorKind;
use std::net::{Ipv4Addr, TcpListener, TcpStream};

use super::{ADDR, PORT, url};

#[test]
fn a_connect_to_it_is_refused() {
    let error = TcpStream::connect(ADDR).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionRefused, "{error}");
}

#[test]
fn a_bind_of_port_zero_never_draws_it() {
    let listeners: Vec<TcpListener> = (0..64)
        .map(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap())
        .collect();
    assert!(
        listeners
            .iter()
            .all(|listener| listener.local_addr().unwrap().port() != PORT)
    );
}

#[test]
fn it_names_its_address_as_an_http_url() {
    assert_eq!(url(), "http://127.0.0.1:1");
}
