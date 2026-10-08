use contract::SessionId;
use contract::events::{Event, TextCompleted};

use super::*;
use crate::Log;

/// A log of one `text_completed` line per entry of `texts`, each with a text
/// of that many bytes, and its offset table read back from the file.
fn written(name: &str, texts: &[usize]) -> (fakes::TempDir, Offsets, Vec<Envelope>) {
    let sessions = fakes::TempDir::new(&format!("log-offsets-{name}"));
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = texts
        .iter()
        .map(|len| {
            let event = Event::TextCompleted(TextCompleted {
                text: "x".repeat(*len),
                provider_item: None,
            });
            log.append(&event, None, None).unwrap()
        })
        .collect();
    drop(log);
    let path = sessions.path().join("s_1").join("events.jsonl");
    let bytes = std::fs::read(&path).unwrap();
    let mut starts = Vec::new();
    let mut start = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        starts.push(start);
        start += u64::try_from(line.len()).unwrap();
    }
    (sessions, Offsets::new(path, starts, start), lines)
}

/// Every page from position 0 on, as long as each says there is more.
fn pages(offsets: &Offsets) -> Vec<(Vec<Envelope>, bool)> {
    let mut pages = Vec::new();
    let mut from = 0;
    loop {
        let page = offsets.page(from, u64::MAX);
        let more = matches!(page.after, After::More);
        from += u64::try_from(page.lines.len()).unwrap();
        pages.push((page.lines, more));
        if !more {
            return pages;
        }
    }
}

fn seqs(lines: &[Envelope]) -> Vec<Option<u64>> {
    lines.iter().map(|line| line.seq.map(|seq| seq.0)).collect()
}

/// A page's lines' `seq`s and whether lines remain past it.
fn shape(page: &Page) -> (Vec<Option<u64>>, bool) {
    (seqs(&page.lines), matches!(page.after, After::More))
}

const KIB: usize = 1024;

#[test]
fn a_page_of_300_kib_lines_holds_three_and_the_pages_return_every_line_once() {
    let (_dir, offsets, written) = written("300k", &[300 * KIB; 10]);
    let pages = pages(&offsets);
    let shape: Vec<(usize, bool)> = pages.iter().map(|(p, more)| (p.len(), *more)).collect();
    assert_eq!(shape, [(3, true), (3, true), (3, true), (1, false)]);
    let read: Vec<Envelope> = pages.into_iter().flat_map(|(p, _)| p).collect();
    assert_eq!(seqs(&read), seqs(&written));
}

#[test]
fn a_line_larger_than_a_page_is_a_page_of_its_own() {
    let (_dir, offsets, written) = written("2m", &[10, 2 * KIB * KIB, 10]);
    assert_eq!(
        shape(&offsets.page(0, u64::MAX)),
        (seqs(&written[..1]), true)
    );
    assert_eq!(
        shape(&offsets.page(1, u64::MAX)),
        (seqs(&written[1..2]), true)
    );
    assert_eq!(
        shape(&offsets.page(2, u64::MAX)),
        (seqs(&written[2..]), false)
    );
}

#[test]
fn a_page_of_small_lines_holds_capacity_lines() {
    let (_dir, offsets, written) = written("small", &[10; CAPACITY + 5]);
    let pages = pages(&offsets);
    let shape: Vec<(usize, bool)> = pages.iter().map(|(p, more)| (p.len(), *more)).collect();
    assert_eq!(shape, [(CAPACITY, true), (5, false)]);
    let read: Vec<Envelope> = pages.into_iter().flat_map(|(p, _)| p).collect();
    assert_eq!(seqs(&read), seqs(&written));
}

#[test]
fn a_log_of_exactly_one_page_has_nothing_more() {
    let (_dir, offsets, written) = written("whole", &[10; CAPACITY]);
    assert_eq!(shape(&offsets.page(0, u64::MAX)), (seqs(&written), false));
}

#[test]
fn a_page_from_the_end_or_past_it_is_empty_with_nothing_more() {
    let (_dir, offsets, _) = written("end", &[10; 3]);
    for from in [3, 4, u64::MAX] {
        assert_eq!(
            shape(&offsets.page(from, u64::MAX)),
            (Vec::new(), false),
            "{from}"
        );
    }
}

#[test]
fn lines_that_fill_a_page_to_the_byte_share_it() {
    let (_dir, empty, _) = written("overhead", &[0]);
    let overhead = usize::try_from(empty.window(0, 1, u64::MAX, u64::MAX).unwrap().stop).unwrap();
    let half = usize::try_from(PAGE_BYTES).unwrap() / 2 - overhead;
    let (_dir, offsets, written) = written("exact", &[half, half, 10]);
    assert_eq!(
        shape(&offsets.page(0, u64::MAX)),
        (seqs(&written[..2]), true)
    );
}

#[test]
fn a_page_never_goes_past_its_end_bound() {
    let (_dir, offsets, written) = written("end-bound", &[10; 5]);
    assert_eq!(shape(&offsets.page(0, 3)), (seqs(&written[..3]), false));
    assert_eq!(shape(&offsets.page(2, 3)), (seqs(&written[2..3]), false));
    for from in [3, 4] {
        assert_eq!(shape(&offsets.page(from, 3)), (Vec::new(), false), "{from}");
    }
    assert_eq!(shape(&offsets.page(0, 0)), (Vec::new(), false));
    assert_eq!(shape(&offsets.page(0, u64::MAX)), (seqs(&written), false));
}

/// Overwrites line `index` (from 0) of the session's log in place with bytes
/// that do not parse, keeping its length, so every offset stays true, and
/// returns the line's original bytes.
fn corrupt(dir: &std::path::Path, index: usize) -> Vec<u8> {
    use std::os::unix::fs::FileExt;
    let path = dir.join("events.jsonl");
    let whole = std::fs::read(&path).unwrap();
    let mut start = 0;
    for line in whole.split_inclusive(|b| *b == b'\n').take(index) {
        start += line.len();
    }
    let len = whole[start..].iter().position(|b| *b == b'\n').unwrap();
    let original = whole[start..start + len].to_vec();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .write_all_at(&vec![b'x'; len], u64::try_from(start).unwrap())
        .unwrap();
    original
}

/// Cuts the session's log at `at` bytes and returns the bytes cut off, so
/// the test can write them back.
fn cut(dir: &std::path::Path, at: usize) -> Vec<u8> {
    let path = dir.join("events.jsonl");
    let whole = std::fs::read(&path).unwrap();
    let cut = whole[at..].to_vec();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(u64::try_from(at).unwrap())
        .unwrap();
    cut
}

/// The byte offset where line `index` (from 0) of `bytes` starts, and the
/// line's length without its newline.
fn line_at(bytes: &[u8], index: usize) -> (usize, usize) {
    let mut start = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n').take(index) {
        start += line.len();
    }
    let len = bytes[start..].iter().position(|b| *b == b'\n').unwrap();
    (start, len)
}

#[test]
fn a_page_over_an_unparseable_line_returns_the_lines_before_it_and_the_error() {
    let (dir, offsets, written) = written("page-bad", &[10; 6]);
    let session = dir.path().join("s_1");
    corrupt(&session, 3);
    let page = offsets.page(0, u64::MAX);
    assert_eq!(seqs(&page.lines), seqs(&written[..3]));
    let After::Failed(error) = page.after else {
        panic!("a page over an unparseable line fails");
    };
    assert!(error.to_string().contains("line 4"), "{error}");
}

#[test]
fn a_page_whose_first_line_does_not_parse_is_empty_and_failed() {
    let (dir, offsets, _) = written("page-bad-first", &[10; 3]);
    let session = dir.path().join("s_1");
    corrupt(&session, 0);
    let page = offsets.page(0, u64::MAX);
    assert!(page.lines.is_empty());
    let After::Failed(error) = page.after else {
        panic!("a page whose first line does not parse fails");
    };
    assert!(error.to_string().contains("line 1"), "{error}");
}

#[test]
fn a_page_over_a_log_cut_short_returns_the_whole_lines_before_the_cut() {
    let (dir, offsets, written) = written("page-cut", &[10; 6]);
    let session = dir.path().join("s_1");
    let whole = std::fs::read(session.join("events.jsonl")).unwrap();
    let (start, len) = line_at(&whole, 3);
    // Inside line 3: only whole lines come before the failure, and the
    // failure is the short read.
    let _cut = cut(&session, start + len / 2);
    let page = offsets.page(0, u64::MAX);
    assert_eq!(seqs(&page.lines), seqs(&written[..3]));
    let After::Failed(Error::Io { source, .. }) = page.after else {
        panic!("a page over a log cut short fails");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::UnexpectedEof);
    // Exactly at the start of line 3: the same lines and the same failure.
    std::fs::write(session.join("events.jsonl"), &whole).unwrap();
    cut(&session, start);
    let page = offsets.page(0, u64::MAX);
    assert_eq!(seqs(&page.lines), seqs(&written[..3]));
    let After::Failed(Error::Io { source, .. }) = page.after else {
        panic!("a page over a log cut short fails");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn a_page_of_a_removed_log_is_empty_and_failed() {
    let (dir, offsets, _) = written("page-gone", &[10; 3]);
    std::fs::remove_file(dir.path().join("s_1").join("events.jsonl")).unwrap();
    let page = offsets.page(0, u64::MAX);
    assert!(page.lines.is_empty());
    let After::Failed(Error::Io { source, .. }) = page.after else {
        panic!("a page of a removed log fails");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn a_parse_error_before_a_cut_is_the_error_returned() {
    let (dir, offsets, written) = written("page-bad-cut", &[10; 6]);
    let session = dir.path().join("s_1");
    corrupt(&session, 1);
    let whole = std::fs::read(session.join("events.jsonl")).unwrap();
    let (start, len) = line_at(&whole, 4);
    let _cut = cut(&session, start + len / 2);
    // The first failure in file order wins: the unparseable line, not the
    // cut after it.
    let page = offsets.page(0, u64::MAX);
    assert_eq!(seqs(&page.lines), seqs(&written[..1]));
    let After::Failed(Error::Unreadable { line, .. }) = page.after else {
        panic!("an unparseable line before a cut fails as unreadable");
    };
    assert_eq!(line, 2);
}

#[test]
fn a_range_over_a_failed_window_is_an_error_with_no_lines() {
    let (dir, offsets, written) = written("range-failed", &[10; 6]);
    let session = dir.path().join("s_1");
    let whole = std::fs::read(session.join("events.jsonl")).unwrap();
    corrupt(&session, 2);
    let err = offsets.range(0, 6).unwrap_err();
    assert!(err.to_string().contains("line 3"), "{err}");
    std::fs::write(session.join("events.jsonl"), &whole).unwrap();
    let (start, len) = line_at(&whole, 3);
    let _cut = cut(&session, start + len / 2);
    let err = offsets.range(0, 6).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    // A window before the damage still reads.
    std::fs::write(session.join("events.jsonl"), &whole).unwrap();
    corrupt(&session, 4);
    assert_eq!(seqs(&offsets.range(0, 2).unwrap()), seqs(&written[..2]));
}
