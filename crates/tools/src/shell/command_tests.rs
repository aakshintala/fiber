use std::io::{self, ErrorKind, Read};
use std::sync::Arc;

use super::{
    Inner, Shared, already_woken, bump, finish, lock, note_eof, poll_while_occupied, read_output,
    suppress_term,
};

#[test]
fn a_moved_sequence_wakes_without_a_cancel() {
    assert!(already_woken(1, 0, false, false));
}

#[test]
fn a_cancel_wakes_only_while_the_run_is_going() {
    assert!(already_woken(0, 0, true, true));
    assert!(!already_woken(0, 0, false, true));
    assert!(!already_woken(0, 0, true, false));
}

#[test]
fn the_drain_polls_only_while_the_group_may_be_occupied() {
    assert!(poll_while_occupied(false));
    assert!(!poll_while_occupied(true));
}

#[test]
fn a_second_signal_and_an_empty_group_are_not_signalled() {
    assert!(suppress_term(true, false));
    assert!(suppress_term(false, true));
    assert!(!suppress_term(false, false));
}

#[test]
fn bump_advances_the_sequence() {
    let mut inner = Inner {
        reaped: false,
        status: None,
        eof: false,
        output: Vec::new(),
        discard: false,
        seq: 0,
    };
    bump(&mut inner);
    assert_eq!(inner.seq, 1);
}

#[test]
fn an_open_pipe_is_discarded_once_the_run_returns() {
    let shared = Shared::default();
    finish(&shared, None, false, true, false);
    assert!(lock(&shared.inner).discard);
}

#[test]
fn an_interrupted_read_is_retried() {
    let shared = Arc::new(Shared::default());
    let reader = Arc::clone(&shared);
    read_output(
        Scripted {
            steps: vec![
                Err(io::Error::new(ErrorKind::Interrupted, "again")),
                Ok(b"hi".to_vec()),
            ],
        },
        &reader,
    );
    let inner = lock(&shared.inner);
    assert_eq!(inner.output, b"hi");
    assert!(inner.eof);
}

#[test]
fn a_read_error_ends_the_output() {
    let shared = Shared::default();
    read_output(
        Scripted {
            steps: vec![Err(io::Error::other("broken"))],
        },
        &shared,
    );
    let inner = lock(&shared.inner);
    assert!(inner.output.is_empty());
    assert!(inner.eof);
}

struct Scripted {
    steps: Vec<io::Result<Vec<u8>>>,
}

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.steps.is_empty() {
            return Ok(0);
        }
        match self.steps.remove(0) {
            Ok(bytes) => {
                let n = bytes.len().min(buf.len());
                if let Some(slot) = buf.get_mut(..n) {
                    slot.copy_from_slice(bytes.get(..n).unwrap_or(&[]));
                }
                Ok(n)
            }
            Err(err) => Err(err),
        }
    }
}

#[test]
fn note_eof_is_idempotent() {
    let shared = Shared::default();
    note_eof(&shared);
    let seq = lock(&shared.inner).seq;
    note_eof(&shared);
    assert_eq!(lock(&shared.inner).seq, seq);
    assert!(lock(&shared.inner).eof);
}
