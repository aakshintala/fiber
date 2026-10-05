//! The output sink's cap and a job's paced `job_delta` lines.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use contract::JobId;
use fakes::TempDir;
use fakes::clock::FakeClock;
use fakes::jobs::JobDeltas;

use super::{JobStream, Shared, lock, read_output};

struct Chunks(Vec<Vec<u8>>);

impl Read for Chunks {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.0.is_empty() {
            return Ok(0);
        }
        let bytes = self.0.remove(0);
        buf[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

fn attached(dir: &TempDir, copy: &[u8], cap: u64) -> (Shared, std::path::PathBuf) {
    let path = dir.path().join("out.log");
    let shared = Shared::default();
    lock(&shared.inner).output.extend_from_slice(copy);
    lock(&shared.inner).attach(std::fs::File::create(&path).unwrap(), cap);
    (shared, path)
}

fn feed(shared: &Shared, chunks: &[&[u8]]) {
    read_output(
        Chunks(chunks.iter().map(|chunk| chunk.to_vec()).collect()),
        shared,
    );
}

#[test]
fn the_file_stops_at_the_cap_and_later_bytes_are_dropped() {
    let dir = TempDir::new("fiber-output-cap");
    let (shared, path) = attached(&dir, b"", 10);
    feed(&shared, &[b"abcdef", b"ghijkl", b"zz"]);
    assert_eq!(std::fs::read(&path).unwrap(), b"abcdefghij");
    let inner = lock(&shared.inner);
    assert!(inner.cap_fired);
    assert_eq!(inner.pending, b"abcdefghij");
    assert_eq!(inner.written, 10);
}

#[test]
fn output_of_exactly_the_cap_does_not_fire_it() {
    let dir = TempDir::new("fiber-output-exact");
    let (shared, path) = attached(&dir, b"", 4);
    feed(&shared, &[b"abcd"]);
    assert!(!lock(&shared.inner).cap_fired);
    feed(&shared, &[b"e"]);
    assert!(lock(&shared.inner).cap_fired);
    assert_eq!(std::fs::read(&path).unwrap(), b"abcd");
}

#[test]
fn a_copy_past_the_cap_is_kept_whole_and_fires_it_at_once() {
    let dir = TempDir::new("fiber-output-copy");
    let (shared, path) = attached(&dir, b"abcdef", 4);
    assert!(lock(&shared.inner).cap_fired);
    feed(&shared, &[b"gh"]);
    assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
    assert!(lock(&shared.inner).pending.is_empty());
}

#[test]
fn a_copy_under_the_cap_leaves_it_unfired_and_counts_toward_it() {
    let dir = TempDir::new("fiber-output-under");
    let (shared, path) = attached(&dir, b"abc", 5);
    assert!(!lock(&shared.inner).cap_fired);
    // The copy is not a delta: the foreground already streamed it.
    assert!(lock(&shared.inner).pending.is_empty());
    feed(&shared, &[b"def"]);
    assert_eq!(std::fs::read(&path).unwrap(), b"abcde");
    assert!(lock(&shared.inner).cap_fired);
}

fn stream() -> (Arc<FakeClock>, Arc<JobDeltas>, JobStream, Shared) {
    let deltas = Arc::new(JobDeltas::default());
    let stream = JobStream::new(JobId("j_x".to_owned()), Arc::clone(&deltas) as _);
    (FakeClock::new(), deltas, stream, Shared::default())
}

fn queue(shared: &Shared, bytes: &[u8]) {
    lock(&shared.inner).pending.extend_from_slice(bytes);
}

fn texts(deltas: &JobDeltas) -> Vec<String> {
    deltas.deltas().into_iter().map(|(_, text)| text).collect()
}

#[test]
fn the_first_change_goes_out_at_once_and_later_ones_collapse() {
    let (clock, deltas, mut stream, shared) = stream();
    queue(&shared, b"one");
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["one"]);
    assert_eq!(deltas.deltas()[0].0, JobId("j_x".to_owned()));
    assert_eq!(stream.deadline(), None);

    queue(&shared, b"two");
    stream.pass(&shared, clock.as_ref());
    queue(&shared, b"three");
    clock.advance(Duration::from_millis(99));
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["one"]);
    assert_eq!(
        stream.deadline(),
        Some(clock.origin() + Duration::from_millis(100))
    );

    clock.advance(Duration::from_millis(1));
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["one", "twothree"]);
    assert_eq!(stream.deadline(), None);
}

#[test]
fn a_large_delta_pushes_the_next_one_out_by_its_size() {
    let (clock, deltas, mut stream, shared) = stream();
    // About 20 KiB encoded: 200 ms at 100 KiB/s, past the 100 ms floor.
    queue(&shared, &vec![b'a'; 20_480]);
    stream.pass(&shared, clock.as_ref());
    queue(&shared, b"b");
    clock.advance(Duration::from_millis(190));
    stream.pass(&shared, clock.as_ref());
    assert_eq!(deltas.deltas().len(), 1);
    let due = stream.deadline().unwrap() - clock.origin();
    assert!(due > Duration::from_millis(190), "{due:?}");
    assert!(due < Duration::from_millis(260), "{due:?}");
    clock.advance(Duration::from_millis(70));
    stream.pass(&shared, clock.as_ref());
    assert_eq!(deltas.deltas().len(), 2);
}

#[test]
fn an_incomplete_character_waits_for_its_other_half() {
    let (clock, deltas, mut stream, shared) = stream();
    let bytes = "é".as_bytes();
    queue(&shared, &[b'a', bytes[0]]);
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["a"]);
    clock.advance(Duration::from_millis(100));
    queue(&shared, &bytes[1..]);
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["a", "é"]);
}

#[test]
fn the_flush_writes_what_is_held_and_an_incomplete_tail_lossily() {
    let (clock, deltas, mut stream, shared) = stream();
    queue(&shared, b"one");
    stream.pass(&shared, clock.as_ref());
    queue(&shared, b"two");
    stream.pass(&shared, clock.as_ref());
    queue(&shared, &[b'!', "é".as_bytes()[0]]);
    stream.flush(&shared);
    assert_eq!(texts(&deltas), ["one", "two!\u{fffd}"]);
    assert!(lock(&shared.inner).pending.is_empty());
    stream.flush(&shared);
    assert_eq!(deltas.deltas().len(), 2);
}

#[test]
fn nothing_pending_emits_nothing() {
    let (clock, deltas, mut stream, shared) = stream();
    stream.pass(&shared, clock.as_ref());
    stream.flush(&shared);
    assert!(deltas.deltas().is_empty());
    assert_eq!(stream.deadline(), None);
}
