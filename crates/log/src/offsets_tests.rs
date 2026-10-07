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
        let (page, more) = offsets.page(from).unwrap();
        from += u64::try_from(page.len()).unwrap();
        pages.push((page, more));
        if !more {
            return pages;
        }
    }
}

fn seqs(lines: &[Envelope]) -> Vec<Option<u64>> {
    lines.iter().map(|line| line.seq.map(|seq| seq.0)).collect()
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
    let (page, more) = offsets.page(0).unwrap();
    assert_eq!((seqs(&page), more), (seqs(&written[..1]), true));
    let (page, more) = offsets.page(1).unwrap();
    assert_eq!((seqs(&page), more), (seqs(&written[1..2]), true));
    let (page, more) = offsets.page(2).unwrap();
    assert_eq!((seqs(&page), more), (seqs(&written[2..]), false));
}

#[test]
fn a_page_of_small_lines_holds_capacity_lines() {
    let (_dir, offsets, written) = written("small", &[10; CAPACITY + 5]);
    let pages = pages(&offsets);
    let shape: Vec<(usize, bool)> = pages.iter().map(|(p, more)| (p.len(), *more)).collect();
    assert_eq!(shape, [(CAPACITY, true), (5, false)]);
    let read: Vec<Envelope> = pages.into_iter().flat_map(|(p, _)| p).collect();
    assert_eq!(read, written);
}

#[test]
fn a_log_of_exactly_one_page_has_nothing_more() {
    let (_dir, offsets, written) = written("whole", &[10; CAPACITY]);
    assert_eq!(offsets.page(0).unwrap(), (written, false));
}

#[test]
fn a_page_from_the_end_or_past_it_is_empty_with_nothing_more() {
    let (_dir, offsets, _) = written("end", &[10; 3]);
    for from in [3, 4, u64::MAX] {
        assert_eq!(offsets.page(from).unwrap(), (Vec::new(), false), "{from}");
    }
}
