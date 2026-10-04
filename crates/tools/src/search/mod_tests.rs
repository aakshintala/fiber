//! Tests beside [`super::Out`]: what a failed write stops.

use std::io::{self, Write};

use super::Out;

/// A writer that fails every write and counts them.
struct Fail {
    writes: usize,
}

impl Write for Fail {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_passes_through_while_writes_succeed() {
    let mut buffer = Vec::new();
    let mut out = Out::new(&mut buffer);
    out.emit(b"a\n");
    out.emit(b"b\n");
    out.flush();
    assert!(!out.broken());
    assert_eq!(buffer, b"a\nb\n");
}

#[test]
fn a_failed_write_stops_later_writes_quietly() {
    let mut fail = Fail { writes: 0 };
    let mut out = Out::new(&mut fail);
    out.emit(b"a\n");
    assert!(out.broken());
    out.emit(b"b\n");
    out.flush();
    assert_eq!(fail.writes, 1);
}

#[test]
fn a_failed_flush_reads_as_a_failed_pipe() {
    struct FailFlush;
    impl Write for FailFlush {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
    }
    let mut fail = FailFlush;
    let mut out = Out::new(&mut fail);
    out.emit(b"a\n");
    assert!(!out.broken());
    out.flush();
    assert!(out.broken());
}
