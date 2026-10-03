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
    let chunk = vec![b'y'; 64 * 1024];
    let cap = 64 * 1024 * 1024;
    let mut wrote = 0usize;
    let mut blocked = false;
    while wrote < cap {
        match server.write(&chunk) {
            Ok(0) => break,
            Ok(n) => wrote += n,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                blocked = true;
                break;
            }
            Err(error) => panic!("{error}"),
        }
    }
    assert!(
        blocked,
        "a paused client reads nothing, so a write blocks before {cap} bytes"
    );

    client.slow(false);
    let line = client
        .recv(Duration::from_secs(2))
        .expect("the line arrives once the client reads");
    assert_eq!(line["ok"], true);
}
