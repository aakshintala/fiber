use std::io::{ErrorKind, Write};
use std::os::unix::net::UnixListener;
use std::time::Duration;

use super::*;
use crate::TempDir;

#[test]
fn a_paused_client_reads_nothing_until_told() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    client.slow(true);
    let (mut server, _) = listener.accept().unwrap();
    server.write_all(b"{\"ok\":true}\n").unwrap();
    server.set_nonblocking(true).unwrap();
    let big = vec![b'y'; 64 * 1024];
    let mut wrote = 0;
    loop {
        match server.write(big.get(wrote..).unwrap_or(&[])) {
            Ok(0) => break,
            Ok(n) => wrote += n,
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => panic!("{error}"),
        }
    }
    assert!(
        wrote < big.len(),
        "a paused client reads nothing, so the socket buffer fills"
    );

    client.slow(false);
    let line = client
        .recv(Duration::from_secs(2))
        .expect("the line arrives once the client reads");
    assert_eq!(line["ok"], true);
}
