use super::{read_line, read_line_partial, write_line};
use std::io::{self, BufRead};

#[path = "../tests/support/framing_input.rs"]
mod framing_input;

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

struct Failing {
    kind: io::ErrorKind,
    calls: u32,
}

impl BufRead for Failing {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.calls > 0 {
            panic!("read_line retried a non-Interrupted fill error");
        }
        self.calls += 1;
        Err(io::Error::new(self.kind, "no retry"))
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
    let mut read = Failing {
        kind: io::ErrorKind::Other,
        calls: 0,
    };
    assert_eq!(
        read_line(&mut read, super::MAX_LINE).unwrap_err().kind(),
        io::ErrorKind::Other
    );
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

#[test]
fn a_fragment_then_error_returns_the_fragment() {
    let mut read =
        framing_input::FragmentThenError::new(br#"{"id":"c_2","command":"prompt"}"#.to_vec(), 1);
    let (error, partial) = read_line_partial(&mut read, super::MAX_LINE).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(partial, b"{".to_vec());
    assert_eq!(read_line_partial(&mut read, super::MAX_LINE).unwrap(), None);
}

#[test]
fn a_body_cut_fragment_returns_every_consumed_byte() {
    let line = br#"{"id":"c_2","command":"prompt"}"#.to_vec();
    let cut = line.len() - 1;
    let mut read = framing_input::FragmentThenError::new(line.clone(), cut);
    let (error, partial) = read_line_partial(&mut read, super::MAX_LINE).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(partial, line[..cut].to_vec());
    assert_eq!(read_line_partial(&mut read, super::MAX_LINE).unwrap(), None);
}

#[test]
fn an_error_with_nothing_consumed_returns_an_empty_partial() {
    let mut read = framing_input::FragmentThenError::new(b"abc".to_vec(), 0);
    let (error, partial) = read_line_partial(&mut read, super::MAX_LINE).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(partial.is_empty());
    assert_eq!(read_line_partial(&mut read, super::MAX_LINE).unwrap(), None);
}

#[test]
fn a_complete_line_matches_the_existing_reader() {
    for bytes in [
        b"{\"id\":\"c_2\"}\n".to_vec(),
        b"a\r\n".to_vec(),
        b"\n".to_vec(),
        b"a\nbc".to_vec(),
        Vec::new(),
    ] {
        let mut old = &bytes[..];
        let mut new = framing_input::FragmentThenError::new(bytes.clone(), bytes.len() + 1);
        assert_eq!(
            read_line_partial(&mut new, super::MAX_LINE).unwrap(),
            read_line(&mut old, super::MAX_LINE).unwrap()
        );
        assert_eq!(
            read_line_partial(&mut new, super::MAX_LINE).unwrap(),
            read_line(&mut old, super::MAX_LINE).unwrap()
        );
    }
}

#[test]
fn an_over_cap_error_matches_the_existing_reader() {
    let mut old = &b"abcd"[..];
    let mut new = framing_input::FragmentThenError::new(b"abcd".to_vec(), 5);
    assert_eq!(
        read_line_partial(&mut new, 3).unwrap_err().0.kind(),
        read_line(&mut old, 3).unwrap_err().kind()
    );
    let (error, partial) = read_line_partial(
        &mut framing_input::FragmentThenError::new(b"abcd".to_vec(), 5),
        3,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(partial.is_empty());
}

#[test]
fn an_interrupted_fill_is_retried_by_the_partial_reader() {
    let mut read = Flaky { interrupted: true };
    assert_eq!(
        read_line_partial(&mut read, super::MAX_LINE).unwrap(),
        Some(b"a".to_vec())
    );
}

#[test]
fn the_stall_fires_below_the_end_and_never_at_or_after_it() {
    let line = b"ab".to_vec();
    for (error_after, want_partial) in [(0, Vec::new()), (1, b"a".to_vec())] {
        let mut read = framing_input::FragmentThenError::new(line.clone(), error_after);
        let (error, partial) = read_line_partial(&mut read, super::MAX_LINE).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(partial, want_partial);
        assert_eq!(read_line_partial(&mut read, super::MAX_LINE).unwrap(), None);
    }
    for error_after in [2, 3] {
        let mut read = framing_input::FragmentThenError::new(line.clone(), error_after);
        assert_eq!(
            read_line_partial(&mut read, super::MAX_LINE).unwrap(),
            Some(b"ab".to_vec())
        );
        assert_eq!(read_line_partial(&mut read, super::MAX_LINE).unwrap(), None);
    }
}

#[test]
fn empty_input_never_stalls() {
    let mut read = framing_input::FragmentThenError::new(Vec::new(), 0);
    assert_eq!(read_line_partial(&mut read, super::MAX_LINE).unwrap(), None);
}

#[test]
fn the_jig_args_default_to_old_mode_and_full_cap() {
    assert_eq!(
        framing_input::parse(
            [
                "input".to_owned(),
                "--error-after".to_owned(),
                "3".to_owned()
            ]
            .into_iter()
        )
        .unwrap(),
        framing_input::Args {
            file: "input".into(),
            error_after: 3,
            max: None,
            mode: framing_input::Mode::Old,
        }
    );
    assert_eq!(
        framing_input::parse(
            [
                "input".to_owned(),
                "--error-after".to_owned(),
                "3".to_owned(),
                "--mode".to_owned(),
                "new".to_owned(),
                "--max".to_owned(),
                "9".to_owned(),
            ]
            .into_iter()
        )
        .unwrap(),
        framing_input::Args {
            file: "input".into(),
            error_after: 3,
            max: Some(9),
            mode: framing_input::Mode::New,
        }
    );
}

#[test]
fn the_jig_args_reject_anything_else() {
    for args in [
        Vec::new(),
        vec!["--error-after", "3"],
        vec!["input"],
        vec!["input", "--error-after"],
        vec!["input", "--error-after", "-1"],
        vec!["input", "--error-after", "x"],
        vec!["input", "--error-after", "3", "--error-after", "4"],
        vec![
            "input",
            "--mode",
            "old",
            "--mode",
            "new",
            "--error-after",
            "3",
        ],
        vec!["input", "--error-after", "3", "--mode", "next"],
        vec!["input", "--error-after", "3", "--max"],
        vec!["input", "--error-after", "3", "extra"],
        vec!["input", "--error-after", "3", "--bogus", "4"],
    ] {
        let parsed = framing_input::parse(args.iter().map(|arg| (*arg).to_owned()));
        assert!(parsed.is_err(), "{args:?}");
    }
}
