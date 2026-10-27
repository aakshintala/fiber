//! The output sink's cap and a job's paced `job_delta` lines.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use contract::JobId;
use fakes::TempDir;
use fakes::clock::FakeClock;
use fakes::jobs::JobDeltas;

use super::{
    Errors, HELD_MAX, JobStream, Pending, Shared, escaped_len, lock, read_errors, read_output,
};

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
    lock(&shared.inner).attach(std::fs::File::create(&path).unwrap(), &path, cap);
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
    assert_eq!(inner.pending.bytes.as_slice(), b"abcdefghij");
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
    assert!(lock(&shared.inner).pending.bytes.is_empty());
}

#[test]
fn a_copy_of_exactly_the_cap_leaves_it_unfired_until_one_more_byte() {
    let dir = TempDir::new("fiber-output-copy-exact");
    let (shared, path) = attached(&dir, b"abcd", 4);
    assert!(!lock(&shared.inner).cap_fired);
    feed(&shared, &[b"e"]);
    assert!(lock(&shared.inner).cap_fired);
    assert_eq!(std::fs::read(&path).unwrap(), b"abcd");
}

#[test]
fn a_copy_under_the_cap_leaves_it_unfired_and_counts_toward_it() {
    let dir = TempDir::new("fiber-output-under");
    let (shared, path) = attached(&dir, b"abc", 5);
    assert!(!lock(&shared.inner).cap_fired);
    // The copy is not a delta: the foreground already streamed it.
    assert!(lock(&shared.inner).pending.bytes.is_empty());
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
    lock(&shared.inner).pending.push(bytes);
}

fn texts(deltas: &JobDeltas) -> Vec<String> {
    deltas.deltas().into_iter().map(|(_, text)| text).collect()
}

/// Asserts `actual` equals `expected` by length and first differing
/// offset, without printing either whole on failure.
fn assert_delta_eq(actual: &str, expected: &str) {
    assert_eq!(actual.len(), expected.len(), "delta length");
    let offset = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(left, right)| left != right);
    assert_eq!(offset, None, "first differing byte offset");
}

/// Asserts `actual` equals `expected` by length and first differing
/// offset, without printing either whole on failure.
fn assert_same_bytes(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len(), "bytes length");
    let offset = actual
        .iter()
        .zip(expected.iter())
        .position(|(left, right)| left != right);
    assert_eq!(offset, None, "first differing byte offset");
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
    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (99 ms before the boundary) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(99));
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["one"]);
    assert_eq!(
        stream.deadline(),
        Some(clock.origin() + Duration::from_millis(100))
    );

    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (100 ms at the boundary) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (190 ms into the large delta) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(190));
    stream.pass(&shared, clock.as_ref());
    assert_eq!(deltas.deltas().len(), 1);
    let due = stream.deadline().unwrap() - clock.origin();
    assert!(due > Duration::from_millis(190), "{due:?}");
    assert!(due < Duration::from_millis(260), "{due:?}");
    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (260 ms past the large delta) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (the incomplete character's 100 ms wait) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
    assert!(lock(&shared.inner).pending.bytes.is_empty());
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

#[test]
fn standard_error_is_held_then_written_to_its_file_up_to_the_cap() {
    let dir = TempDir::new("fiber-shell-errors");
    let path = dir.path().join("err.log");
    let shared = Shared::default();
    lock(&shared.inner).errors = Some(Errors::default());
    read_errors(Chunks(vec![b"ab".to_vec()]), &shared);
    {
        let mut inner = lock(&shared.inner);
        let errors = inner.errors.as_mut().unwrap();
        assert!(errors.eof);
        errors.attach(Some(std::fs::File::create(&path).unwrap()), 5);
        errors.sink(b"cdef");
        errors.sink(b"g");
    }
    // Past the cap the bytes are dropped; nothing stops.
    assert_eq!(std::fs::read(&path).unwrap(), b"abcde");
    assert!(!lock(&shared.inner).cap_fired);
}

#[test]
fn standard_error_with_no_file_is_dropped_after_the_move() {
    let shared = Shared::default();
    lock(&shared.inner).errors = Some(Errors::default());
    read_errors(Chunks(vec![b"before".to_vec()]), &shared);
    let mut inner = lock(&shared.inner);
    let errors = inner.errors.as_mut().unwrap();
    assert_eq!(errors.held, b"before", "held until the move");
    errors.attach(None, 5);
    errors.sink(b"after");
    assert!(errors.held.is_empty(), "nothing is held past the move");
    assert_eq!(errors.written, 0);
}

#[test]
fn standard_error_reaches_no_line_and_no_delta() {
    let shared = Shared::default();
    {
        let mut inner = lock(&shared.inner);
        inner.errors = Some(Errors::default());
        inner.lines = Some(Vec::new());
    }
    read_errors(Chunks(vec![b"secret\n".to_vec()]), &shared);
    let inner = lock(&shared.inner);
    assert!(inner.lines.as_ref().unwrap().is_empty());
    assert!(inner.pending.bytes.is_empty());
    assert!(inner.output.is_empty());
}

#[test]
fn the_end_of_output_waits_for_both_streams() {
    let shared = Shared::default();
    lock(&shared.inner).errors = Some(Errors::default());
    read_output(Chunks(Vec::new()), &shared);
    assert!(lock(&shared.inner).eof);
    assert!(
        !lock(&shared.inner).all_eof(),
        "standard error is still open"
    );
    read_errors(Chunks(Vec::new()), &shared);
    assert!(lock(&shared.inner).all_eof());
    // Without a separate standard error, standard output alone decides.
    let plain = Shared::default();
    assert!(!lock(&plain.inner).all_eof());
    read_output(Chunks(Vec::new()), &plain);
    assert!(lock(&plain.inner).all_eof());
}

#[test]
fn a_monitors_output_is_kept_for_its_lines_from_the_move_on() {
    let dir = TempDir::new("fiber-shell-lines");
    let path = dir.path().join("out.log");
    let shared = Shared::default();
    {
        let mut inner = lock(&shared.inner);
        inner.lines = Some(Vec::new());
        inner.output = b"before\n".to_vec();
        inner.attach(std::fs::File::create(&path).unwrap(), &path, 100);
    }
    read_output(Chunks(vec![b"after\n".to_vec()]), &shared);
    assert_eq!(
        lock(&shared.inner).lines.as_deref(),
        Some(&b"before\nafter\n"[..])
    );
    // Any other job keeps none.
    let plain = Shared::default();
    let job = dir.path().join("p.log");
    lock(&plain.inner).attach(std::fs::File::create(&job).unwrap(), &job, 100);
    read_output(Chunks(vec![b"x\n".to_vec()]), &plain);
    assert!(lock(&plain.inner).lines.is_none());
}

#[test]
fn the_pace_is_the_encoded_event_length() {
    use contract::events::{Event, JobDelta, Progress};
    let (clock, deltas, mut stream, shared) = stream();
    // Mixed escapes, past 10 KiB raw: the pace sits above the 100 ms floor.
    let text = "a\"\n\u{1}é".repeat(2000);
    queue(&shared, text.as_bytes());
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), [text.as_str()]);
    let event = Event::JobDelta(JobDelta {
        job_id: JobId("j_x".to_owned()),
        progress: Progress {
            text: Some(text),
            details: None,
        },
    });
    let encoded = u64::try_from(serde_json::to_vec(&event).unwrap().len()).unwrap();
    let paced = Duration::from_nanos(encoded.saturating_mul(1_000_000_000) / 102_400)
        .max(Duration::from_millis(100));
    queue(&shared, b"b");
    stream.pass(&shared, clock.as_ref());
    assert_eq!(stream.deadline(), Some(clock.origin() + paced));
}

#[test]
fn escaped_len_matches_serde_json_for_each_escape_class() {
    let cases = [
        "plain ASCII",
        "\"",
        "\\",
        "\u{8}",
        "\u{c}",
        "\n",
        "\r",
        "\t",
        "\u{0}",
        "\u{1}",
        "\u{1f}",
        "\u{7f}",
        "é",
        "😀",
        "a\"\n\u{1}é",
    ];
    for case in cases {
        assert_eq!(
            escaped_len(case),
            u64::try_from(serde_json::to_string(case).unwrap().len() - 2).unwrap(),
            "{case:?}"
        );
    }
}

use proptest::prelude::*;

proptest! {
    #[test]
    fn escaped_len_matches_serde_json_on_any_string(text: String) {
        prop_assert_eq!(
            escaped_len(&text),
            u64::try_from(serde_json::to_string(&text).unwrap().len() - 2).unwrap()
        );
    }
}

#[test]
fn a_queue_of_exactly_64_kib_sends_it_all_in_one_delta() {
    let (clock, deltas, mut stream, shared) = stream();
    queue(&shared, &vec![b'a'; 65_536]);
    stream.pass(&shared, clock.as_ref());
    let got = texts(&deltas);
    assert_eq!(got.len(), 1);
    assert_delta_eq(&got[0], &"a".repeat(65_536));
}

#[test]
fn a_queue_of_one_byte_past_64_kib_sends_the_last_64_kib() {
    let (clock, deltas, mut stream, shared) = stream();
    let mut queued = vec![b'X'];
    queued.extend_from_slice(&vec![b'a'; 65_536]);
    queue(&shared, &queued);
    stream.pass(&shared, clock.as_ref());
    let got = texts(&deltas);
    assert_eq!(got.len(), 1);
    assert_delta_eq(&got[0], &"a".repeat(65_536));
}

#[test]
fn a_queue_cut_inside_a_two_byte_character_starts_at_the_next_lead_byte() {
    let (clock, deltas, mut stream, shared) = stream();
    let mut queued = "é".as_bytes().to_vec();
    queued.extend_from_slice(&vec![b'a'; 65_535]);
    assert_eq!(queued.len(), 65_537);
    queue(&shared, &queued);
    stream.pass(&shared, clock.as_ref());
    let got = texts(&deltas);
    assert_eq!(got.len(), 1);
    assert_delta_eq(&got[0], &"a".repeat(65_535));
    assert!(!got[0].contains('\u{fffd}'), "no replacement character");
}

#[test]
fn a_dropped_queue_discards_the_carried_tail() {
    let (clock, deltas, mut stream, shared) = stream();
    queue(&shared, &[b'a', "é".as_bytes()[0]]);
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["a"]);
    queue(&shared, &vec![b'b'; 65_537]);
    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (the dropped queue's 100 ms tail) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(100));
    stream.pass(&shared, clock.as_ref());
    let got = texts(&deltas);
    assert_eq!(got.len(), 2);
    assert_delta_eq(&got[1], &"b".repeat(65_536));
    assert!(!got[1].contains('\u{fffd}'), "no replacement character");
}

#[test]
fn a_kept_queue_keeps_the_carried_tail() {
    let (clock, deltas, mut stream, shared) = stream();
    queue(&shared, &[b'a', "é".as_bytes()[0]]);
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["a"]);
    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (the kept queue's 100 ms tail) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(100));
    queue(&shared, &"é".as_bytes()[1..]);
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["a", "é"]);
}

#[test]
fn held_text_across_not_yet_due_passes_is_capped_at_the_newest_64_kib() {
    let (clock, deltas, mut stream, shared) = stream();
    queue(&shared, b"one");
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["one"]);
    queue(&shared, &vec![b'a'; 40_000]);
    stream.pass(&shared, clock.as_ref());
    queue(&shared, &vec![b'b'; 40_000]);
    stream.pass(&shared, clock.as_ref());
    assert_eq!(texts(&deltas), ["one"], "nothing due yet");
    // No code under test waits on the clock here; JobStream::pass is a direct call that only reads the clock (the capped tail's 100 ms wait) (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(100));
    stream.pass(&shared, clock.as_ref());
    let got = texts(&deltas);
    assert_eq!(got.len(), 2);
    let mut expected = "a".repeat(25_536);
    expected.push_str(&"b".repeat(40_000));
    assert_delta_eq(&got[1], &expected);
}

#[test]
fn a_pending_push_of_exactly_64_kib_keeps_everything() {
    let mut pending = Pending::default();
    pending.push(&vec![b'a'; HELD_MAX]);
    assert!(!pending.dropped);
    assert_same_bytes(&pending.bytes, &vec![b'a'; HELD_MAX]);
}

#[test]
fn a_pending_push_of_one_byte_past_64_kib_drops_the_first_byte() {
    let mut pending = Pending::default();
    let mut over = vec![b'X'];
    over.extend_from_slice(&vec![b'a'; HELD_MAX]);
    pending.push(&over);
    assert!(pending.dropped);
    assert_same_bytes(&pending.bytes, &vec![b'a'; HELD_MAX]);
}

#[test]
fn a_pending_cut_skips_at_most_three_leading_continuation_bytes() {
    for n in 0..=4_usize {
        let mut pending = Pending::default();
        let mut queued = vec![b'X'; 5];
        queued.extend_from_slice(&vec![0x80; n]);
        queued.extend_from_slice(&vec![b'a'; HELD_MAX - n]);
        assert_eq!(queued.len(), HELD_MAX + 5);
        pending.push(&queued);
        assert!(pending.dropped, "n = {n}");
        let skipped = n.min(3);
        let mut expected = vec![0x80; n - skipped];
        expected.extend_from_slice(&vec![b'a'; HELD_MAX - n]);
        assert_same_bytes(&pending.bytes, &expected);
    }
}

#[test]
fn a_pending_cut_skips_only_continuation_bytes() {
    for (lead, skipped) in [
        (0x41_u8, false),
        (0xC3_u8, false),
        (0x80_u8, true),
        (0xBF_u8, true),
    ] {
        let mut pending = Pending::default();
        let mut queued = vec![b'X'; 5];
        queued.extend_from_slice(&[lead]);
        queued.extend_from_slice(&vec![b'a'; HELD_MAX - 1]);
        assert_eq!(queued.len(), HELD_MAX + 5);
        pending.push(&queued);
        assert!(pending.dropped, "lead = {lead:#X}");
        let mut expected = Vec::new();
        if !skipped {
            expected.push(lead);
        }
        expected.extend_from_slice(&vec![b'a'; HELD_MAX - 1]);
        assert_same_bytes(&pending.bytes, &expected);
    }
}

#[test]
fn two_pending_pushes_past_the_cap_keep_the_last_64_kib_in_order() {
    let mut pending = Pending::default();
    pending.push(&vec![b'a'; 40_000]);
    assert!(!pending.dropped);
    pending.push(&vec![b'b'; 40_000]);
    assert!(pending.dropped);
    let mut expected = vec![b'a'; 25_536];
    expected.extend_from_slice(&vec![b'b'; 40_000]);
    assert_same_bytes(&pending.bytes, &expected);
}

#[test]
fn a_pending_take_clears_both_bytes_and_flag() {
    let mut pending = Pending::default();
    pending.push(b"abc");
    let (bytes, dropped) = pending.take();
    assert_eq!(bytes, b"abc");
    assert!(!dropped);
    assert!(pending.bytes.is_empty());
    let (bytes, dropped) = pending.take();
    assert!(bytes.is_empty());
    assert!(!dropped);
    let mut over = vec![b'X'];
    over.extend_from_slice(&vec![b'a'; HELD_MAX]);
    pending.push(&over);
    let (bytes, dropped) = pending.take();
    assert!(dropped);
    assert_same_bytes(&bytes, &vec![b'a'; HELD_MAX]);
    assert!(pending.bytes.is_empty());
    let (_, dropped) = pending.take();
    assert!(!dropped);
}

use super::super::spill::{Config as SpillConfig, SPILL_AT};
use super::check_start;
use contract::tool::Bound;

/// Arms `shared`'s spill into `dir`: the reader routes past 1 MiB there.
fn arm(shared: &Shared, dir: &TempDir) {
    lock(&shared.inner).spill_config = Some(SpillConfig {
        dir: dir.path().to_path_buf(),
        bound: Bound {
            start: 8192,
            end: 8192,
        },
        wrap: None,
        name: None,
    });
}

/// `n` bytes in 8 KiB pieces, the reader's own read size.
fn pieces(n: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut left = n;
    while left > 0 {
        let take = left.min(8192);
        out.push(vec![b'y'; take]);
        left -= take;
    }
    out
}

#[test]
fn one_byte_short_of_a_mebibyte_never_spills() {
    let artifacts = TempDir::new("fiber-spill-under-artifacts");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    let owned = pieces(SPILL_AT - 1);
    let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    feed(&shared, &refs);
    assert!(lock(&shared.inner).spill.is_none());
    assert_eq!(lock(&shared.inner).output.len(), SPILL_AT - 1);
    assert!(
        std::fs::read_dir(artifacts.path())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn exactly_a_mebibyte_never_spills() {
    let artifacts = TempDir::new("fiber-spill-exact-artifacts");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    let owned = pieces(SPILL_AT);
    let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    feed(&shared, &refs);
    assert!(lock(&shared.inner).spill.is_none());
    assert_eq!(lock(&shared.inner).output.len(), SPILL_AT);
    assert!(
        std::fs::read_dir(artifacts.path())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn one_byte_past_a_mebibyte_spills_from_the_first_byte() {
    let artifacts = TempDir::new("fiber-spill-over-artifacts");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    let owned = pieces(SPILL_AT + 1);
    let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    feed(&shared, &refs);
    assert!(lock(&shared.inner).spill.is_some());
    let inner = lock(&shared.inner);
    assert!(inner.output.is_empty());
    assert_eq!(inner.output.capacity(), 0);
    drop(inner);
    let entries: Vec<_> = std::fs::read_dir(artifacts.path()).unwrap().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        std::fs::read(entries[0].as_ref().unwrap().path())
            .unwrap()
            .len(),
        SPILL_AT + 1
    );
}

#[test]
fn sixty_four_mebibytes_leave_only_the_ends_and_the_capped_queue() {
    let artifacts = TempDir::new("fiber-spill-64-artifacts");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    read_output(io::repeat(b'y').take(64 << 20), &shared);
    let inner = lock(&shared.inner);
    assert!(inner.spill.is_some());
    assert!(inner.output.is_empty());
    assert_eq!(inner.output.capacity(), 0);
    let spill = inner.spill.as_ref().unwrap();
    assert!(spill.head.len() <= 8192);
    assert!(spill.ring.bytes.len() <= 8192);
    assert!(inner.stream.bytes.len() <= HELD_MAX);
    assert_eq!(spill.total, 64 << 20);
    drop(inner);
    let entries: Vec<_> = std::fs::read_dir(artifacts.path()).unwrap().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        std::fs::read(entries[0].as_ref().unwrap().path())
            .unwrap()
            .len(),
        64 << 20
    );
}

#[test]
fn without_an_armed_spill_the_buffer_is_unbounded() {
    let shared = Shared::default();
    let owned = pieces(SPILL_AT + 1);
    let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    feed(&shared, &refs);
    assert!(lock(&shared.inner).spill.is_none());
    assert_eq!(lock(&shared.inner).output.len(), SPILL_AT + 1);
}

#[test]
fn check_start_arms_nothing_without_a_configuration() {
    let shared = Shared::default();
    {
        let mut inner = lock(&shared.inner);
        inner.output = vec![b'y'; SPILL_AT + 1];
        check_start(&mut inner, 0);
        assert!(inner.spill.is_none());
    }
}

#[test]
fn check_start_keeps_a_spill_already_in_progress() {
    let artifacts = TempDir::new("fiber-spill-keep");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    let owned = pieces(SPILL_AT + 100);
    let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    feed(&shared, &refs);
    let total = lock(&shared.inner).spill.as_ref().unwrap().total;
    {
        let mut inner = lock(&shared.inner);
        // Bytes buffered behind the spill start no second one.
        inner.output = vec![b'z'; SPILL_AT + 1];
        check_start(&mut inner, 0);
        let spill = inner.spill.as_ref().unwrap();
        assert_eq!(spill.total, total);
        assert_eq!(spill.head[..100], [b'y'; 100]);
        assert_eq!(inner.output.len(), SPILL_AT + 1);
    }
}

#[test]
fn attach_without_a_spill_copies_whole_and_returns_no_line() {
    let artifacts = TempDir::new("fiber-spill-attach-artifacts");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    let owned = pieces(1000);
    let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    feed(&shared, &refs);
    // Under the threshold the whole-copy path still returns no line.
    let job = artifacts.path().join("j_1.log");
    let line = lock(&shared.inner).attach(std::fs::File::create(&job).unwrap(), &job, 10);
    assert_eq!(line, None);
    assert_eq!(std::fs::read(&job).unwrap().len(), 1000);
}

#[test]
fn attach_after_a_spill_moves_the_file_into_the_job() {
    let artifacts = TempDir::new("fiber-spill-attach-spilled");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    let owned = pieces(SPILL_AT + 100);
    let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
    feed(&shared, &refs);
    let job = artifacts.path().join("j_1.log");
    let line =
        lock(&shared.inner).attach(std::fs::File::create(&job).unwrap(), &job, 5_000_000_000);
    assert_eq!(line, None);
    let inner = lock(&shared.inner);
    assert_eq!(inner.written, (SPILL_AT + 100) as u64);
    assert!(!inner.cap_fired);
    drop(inner);
    assert_eq!(std::fs::read(&job).unwrap().len(), SPILL_AT + 100);
    // Later output follows into the job's file.
    feed(&shared, &[b"more"]);
    assert_eq!(std::fs::read(&job).unwrap().len(), SPILL_AT + 104);
}

#[test]
fn attach_after_a_spill_with_a_cap_below_its_size_fires_at_once() {
    let artifacts = TempDir::new("fiber-spill-attach-capped");
    let shared = Shared::default();
    arm(&shared, &artifacts);
    // A spill started directly, past the tiny cap the attach carries.
    {
        let mut inner = lock(&shared.inner);
        let config = inner.spill_config.clone().unwrap();
        inner.spill = Some(super::super::spill::Spill::start(config, vec![b'y'; 100]));
    }
    let job = artifacts.path().join("j_1.log");
    lock(&shared.inner).attach(std::fs::File::create(&job).unwrap(), &job, 50);
    let inner = lock(&shared.inner);
    assert_eq!(inner.written, 100);
    assert!(inner.cap_fired);
}

#[test]
fn attach_after_a_spill_at_the_cap_boundaries() {
    for (cap, fired) in [(99_u64, true), (100_u64, false), (101_u64, false)] {
        let artifacts = TempDir::new(&format!("fiber-spill-attach-exact-{cap}"));
        let shared = Shared::default();
        arm(&shared, &artifacts);
        // A spill started directly, holding exactly 100 bytes.
        {
            let mut inner = lock(&shared.inner);
            let config = inner.spill_config.clone().unwrap();
            inner.spill = Some(super::super::spill::Spill::start(config, vec![b'y'; 100]));
        }
        let job = artifacts.path().join("j_1.log");
        lock(&shared.inner).attach(std::fs::File::create(&job).unwrap(), &job, cap);
        let inner = lock(&shared.inner);
        assert_eq!(inner.written, 100);
        assert_eq!(inner.cap_fired, fired, "cap = {cap}");
    }
}
