use super::{read_line, write_line};
use std::io::{self, BufRead};

#[test]
fn an_lf_ends_a_line_and_eof_ends_the_input() {
    let mut read = &b"a\nb\n"[..];
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"a".to_vec())
    );
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"b".to_vec())
    );
    assert_eq!(read_line(&mut read, super::MAX_LINE).unwrap(), None);
}

#[test]
fn a_crlf_ends_a_line() {
    let mut read = &b"a\r\nb\r\n"[..];
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"a".to_vec())
    );
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"b".to_vec())
    );
    assert_eq!(read_line(&mut read, super::MAX_LINE).unwrap(), None);
}

#[test]
fn trailing_cr_bytes_are_stripped_before_the_lf() {
    let mut read = &b"a\r\r\n"[..];
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"a".to_vec())
    );
}

#[test]
fn an_empty_line_is_some_empty() {
    let mut read = &b"\n"[..];
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(Vec::new())
    );
}

#[test]
fn a_torn_final_line_is_returned_then_none() {
    let mut read = &b"a\nbc"[..];
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"a".to_vec())
    );
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"bc".to_vec())
    );
    assert_eq!(read_line(&mut read, super::MAX_LINE).unwrap(), None);
}

struct Flaky {
    interrupted: bool,
}

impl BufRead for Flaky {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.interrupted {
            self.interrupted = false;
            Err(io::Error::new(io::ErrorKind::Interrupted, "a signal"))
        } else {
            Ok(b"a\n")
        }
    }

    fn consume(&mut self, _amount: usize) {}
}

impl std::io::Read for Flaky {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        unreachable!()
    }
}

struct Failing(io::ErrorKind);

impl BufRead for Failing {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Err(io::Error::new(self.0, "no retry"))
    }

    fn consume(&mut self, _amount: usize) {}
}

impl std::io::Read for Failing {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        unreachable!()
    }
}

#[test]
fn an_interrupted_fill_is_retried() {
    let mut read = Flaky { interrupted: true };
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap(),
        Some(b"a".to_vec())
    );
}

#[test]
fn any_other_fill_error_is_returned() {
    let mut read = Failing(io::ErrorKind::Other);
    assert!(read_line(&mut read, super::MAX_LINE).is_err());
}

#[test]
fn a_line_at_the_cap_is_read_and_one_over_fails() {
    let mut read = &b"abc\n"[..];
    assert_eq!(read_line(&mut read, 3).unwrap(), Some(b"abc".to_vec()));
    let mut read = &b"abcd\n"[..];
    assert_eq!(
        read_line(&mut read, 3).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut read = &b"abc"[..];
    assert_eq!(read_line(&mut read, 3).unwrap(), Some(b"abc".to_vec()));
    let mut read = &b"abcd"[..];
    assert_eq!(
        read_line(&mut read, 3).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut read = &b"ab\n"[..];
    assert_eq!(read_line(&mut read, 3).unwrap(), Some(b"ab".to_vec()));
}

#[test]
fn the_cap_counts_a_cr_before_the_lf() {
    let mut read = &b"abc\r\n"[..];
    assert_eq!(
        read_line(&mut read, 3).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[derive(Default)]
struct Counting {
    writes: Vec<Vec<u8>>,
    flushes: usize,
}

impl std::io::Write for Counting {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes.push(buf.to_vec());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn write_line_appends_one_lf_and_flushes() {
    let mut out = Counting::default();
    write_line(&mut out, b"x").unwrap();
    assert_eq!(out.writes, vec![b"x\n".to_vec()]);
    assert_eq!(out.flushes, 1);
}
