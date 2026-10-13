use contract::SessionId;
use contract::events::{Event, TextCompleted};

use super::*;
use crate::Log;
use crate::fixtures::{corrupt, cut, line, line_at};

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
    let overhead = usize::try_from(
        empty
            .resolve(0, 1, u64::MAX, u64::MAX)
            .unwrap()
            .unwrap()
            .stop,
    )
    .unwrap();
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

/// A table that never drops a line start grows with every line: after
/// more pushes than the cap of 1024 resident checkpoints, the resident
/// set must re-thin to at most half the cap and stay capped.
#[test]
fn resident_checkpoints_stay_at_a_constant_cap() {
    let offsets = Offsets::new(PathBuf::from("red-cap"), Vec::new(), 0);
    assert!(offsets.lock().checkpoints.is_empty());
    for _ in 0..1023 {
        offsets.push(100);
    }
    assert_eq!(offsets.lock().checkpoints.len(), 1023);
    assert_eq!(offsets.lock().checkpoints[0], (0, 0));
    offsets.push(100);
    assert_eq!(offsets.lock().checkpoints.len(), 1024);
    assert_eq!(offsets.lock().checkpoints[0], (0, 0));
    offsets.push(100);
    // One past the cap the table re-thins to at most half the cap. The
    // doubling passes keep every line while the spacing stays below a
    // line: spacing 128 keeps every other line (513, still over half),
    // then spacing 256 re-walks those and keeps every fourth (400-byte
    // gaps, 257).
    assert!(offsets.lock().checkpoints.len() <= CHECKPOINTS / 2);
    assert_eq!(offsets.lock().checkpoints.len(), 257);
    assert_eq!(offsets.lock().checkpoints[0], (0, 0));
    // Re-thinning allocates the compacted set instead of retaining the
    // grown buffer: right after the thin the capacity sits at the cap,
    // not at the log's scale.
    assert!(offsets.lock().checkpoints.capacity() <= CHECKPOINTS);
    // The absolute line count never depends on checkpointing.
    assert_eq!(offsets.count(), 1025);
}

/// While the log holds no more lines than the cap, every line start is
/// resident: small logs read exactly as they do today.
#[test]
fn small_tables_checkpoint_every_line() {
    let offsets = Offsets::new(PathBuf::from("red-small"), Vec::new(), 0);
    for _ in 0..10 {
        offsets.push(100);
    }
    let table = offsets.lock();
    assert_eq!(table.checkpoints.len(), 10);
    for (i, checkpoint) in table.checkpoints.iter().enumerate() {
        assert_eq!(
            *checkpoint,
            (i as u64, (i as u64).saturating_mul(100)),
            "line {i}"
        );
    }
}

#[test]
fn scan_records_every_complete_line_and_stops_at_a_torn_tail() {
    // A torn tail is not a line: appending torn bytes leaves the table
    // unchanged.
    let (dir, offsets, lines) = written("scan-whole", &[10; 3]);
    let session = dir.path().join("s_1");
    let scanned = Offsets::scan(&session, u64::MAX, |_| Ok(())).unwrap();
    {
        let expected = offsets.lock();
        let found = scanned.lock();
        assert_eq!(found.checkpoints, expected.checkpoints);
        assert_eq!(found.end, expected.end);
    }
    assert_eq!(scanned.count(), 3);
    assert_eq!(seqs(&scanned.range(0, 10).unwrap()), seqs(&lines));
    assert_eq!(shape(&scanned.page(0, u64::MAX)).0, seqs(&lines));
    drop(dir);
    let (dir, offsets, _) = written("scan-tail", &[10; 3]);
    let session = dir.path().join("s_1");
    let mut bytes = std::fs::read(session.join("events.jsonl")).unwrap();
    bytes.extend_from_slice(b"{\"torn");
    std::fs::write(session.join("events.jsonl"), &bytes).unwrap();
    let scanned = Offsets::scan(&session, u64::MAX, |_| Ok(())).unwrap();
    {
        let expected = offsets.lock();
        let found = scanned.lock();
        assert_eq!(found.checkpoints, expected.checkpoints);
        assert_eq!(found.end, expected.end);
    }
    assert_eq!(scanned.count(), 3);
}

#[test]
fn scan_reads_at_most_limit_lines() {
    let (dir, _, _) = written("scan-limit", &[10; 5]);
    let session = dir.path().join("s_1");
    for (limit, expected) in [(0, 0), (2, 2), (4, 4), (5, 5), (6, 5)] {
        let scanned = Offsets::scan(&session, limit, |_| Ok(())).unwrap();
        assert_eq!(scanned.count(), expected, "limit {limit}");
        assert_eq!(
            scanned.range(0, 100).unwrap().len() as u64,
            expected,
            "limit {limit}"
        );
    }
}

#[test]
fn scan_parses_nothing() {
    let (dir, _, _) = written("scan-corrupt", &[10; 3]);
    let session = dir.path().join("s_1");
    corrupt(&session, 1);
    let scanned = Offsets::scan(&session, u64::MAX, |_| Ok(())).unwrap();
    assert_eq!(scanned.count(), 3);
    let one = Offsets::scan(&session, 1, |_| Ok(())).unwrap();
    assert_eq!(one.count(), 1);
}

#[test]
fn scan_of_a_missing_log_is_not_found() {
    let home = fakes::TempDir::new("log-offsets-scan-missing");
    let missing = home.path().join("s_1");
    let error = match Offsets::scan(&missing, u64::MAX, |_| Ok(())) {
        Ok(_) => panic!("a missing log is not found"),
        Err(error) => error,
    };
    assert!(
        matches!(&error, Error::NotFound(path) if *path == missing),
        "{error}"
    );
}

#[test]
fn scan_stops_at_the_first_line_its_closure_refuses_and_names_it() {
    let (dir, _, _) = written("scan-refuse", &[10; 5]);
    let session = dir.path().join("s_1");
    let mut seen = 0;
    let error = match Offsets::scan(&session, u64::MAX, |_| {
        seen += 1;
        if seen == 3 {
            Err(serde::de::Error::custom("refused"))
        } else {
            Ok(())
        }
    }) {
        Ok(_) => panic!("a refused line ends the scan"),
        Err(error) => error,
    };
    let Error::Unreadable { line, .. } = &error else {
        panic!("a refused line is unreadable: {error}");
    };
    assert_eq!(*line, 3);
    assert_eq!(seen, 3);
}

/// A table driven directly through `push`, counting thinning events: a
/// thinning drops the resident set from above 900 to at most half the cap.
fn driven(lens: &[u64]) -> (Offsets, u64) {
    let offsets = Offsets::new(PathBuf::from("log-offsets-driven"), Vec::new(), 0);
    let mut thins = 0;
    let mut was_full = false;
    for len in lens {
        offsets.push(*len);
        let resident = offsets.lock().checkpoints.len();
        if resident > 900 {
            was_full = true;
        }
        if was_full && resident <= CHECKPOINTS / 2 {
            thins += 1;
            was_full = false;
        }
    }
    (offsets, thins)
}

/// Every gap of a thinned table holds within its spacing: at least the
/// spacing past the last kept checkpoint, and at most twice the spacing
/// plus the longest line the gap holds.
fn assert_gaps(checkpoints: &[(u64, u64)], spacing: u64, lens: &[u64]) {
    assert!(checkpoints.len() > 1);
    for pair in checkpoints.windows(2) {
        let (a_line, a_byte) = pair[0];
        let (b_line, b_byte) = pair[1];
        let gap = b_byte.saturating_sub(a_byte);
        assert!(gap >= spacing, "gap {gap} below spacing {spacing}");
        let mut longest = 0;
        let mut line = a_line;
        while line < b_line {
            if let Ok(i) = usize::try_from(line) {
                longest = longest.max(*lens.get(i).unwrap_or(&0));
            }
            line = line.saturating_add(1);
            if line == u64::MAX {
                break;
            }
        }
        assert!(
            gap <= spacing.saturating_mul(2).saturating_add(longest),
            "gap {gap} over twice spacing {spacing} plus longest {longest}"
        );
    }
}

/// The spacing stays within the initial spacing and four bytes per
/// resident byte of log: re-thinning doubles only past what the log fills.
fn assert_spacing(spacing: u64, end: u64) {
    assert!(
        spacing <= 1u64.max(end.saturating_mul(4) / 1024),
        "spacing {spacing} over bound at end {end}"
    );
}

#[test]
fn re_thinning_bounds_gaps_across_many_thinnings() {
    // Thirty thousand pushes drive several thinnings eight-plus
    // doublings deep: triggers grow geometrically costlier as the
    // spacing grows, so depth is read off the spacing, not the trigger
    // count. Eight triggers would take a gigabyte-scale log.
    let lens = vec![200u64; 30_000];
    let (offsets, thins) = driven(&lens);
    assert!(thins >= 1, "{thins} thinnings");
    let table = offsets.lock();
    assert!(
        table.spacing.trailing_zeros() >= 8,
        "spacing {}",
        table.spacing
    );
    assert!(table.checkpoints.len() <= CHECKPOINTS);
    assert_eq!(table.total, 30_000);
    assert_eq!(table.checkpoints[0], (0, 0));
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
    // Re-thinning reallocates the compacted set instead of retaining the
    // grown buffer: capacity stays near the cap, not the log.
    assert!(table.checkpoints.capacity() <= 2 * CHECKPOINTS);
}

#[test]
fn re_thinning_holds_early_dense_logs() {
    let mut lens = vec![10_000u64; 2000];
    lens.extend(vec![200u64; 4000]);
    let (offsets, _thins) = driven(&lens);
    let table = offsets.lock();
    // One thinning already doubles the spacing past eight doublings: the
    // repeat passes keep doubling until at most half the cap remains.
    assert!(
        table.spacing.trailing_zeros() >= 8,
        "spacing {}",
        table.spacing
    );
    assert!(table.checkpoints.len() <= CHECKPOINTS);
    assert_eq!(table.total, 6000);
    assert_eq!(table.checkpoints[0], (0, 0));
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

#[test]
fn re_thinning_holds_uniform_long_lines() {
    let lens = vec![10_000u64; 7500];
    let (offsets, _thins) = driven(&lens);
    let table = offsets.lock();
    assert!(
        table.spacing.trailing_zeros() >= 8,
        "spacing {}",
        table.spacing
    );
    assert!(table.checkpoints.len() <= CHECKPOINTS);
    assert_eq!(table.total, 7500);
    assert_eq!(table.end, 75_000_000);
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

#[test]
fn re_thinning_holds_alternating_lines() {
    let mut lens = Vec::new();
    for _ in 0..6000 {
        lens.push(50);
        lens.push(5000);
    }
    let (offsets, thins) = driven(&lens);
    assert!(thins >= 1, "{thins} thinnings");
    let table = offsets.lock();
    assert!(table.checkpoints.len() <= CHECKPOINTS);
    assert_eq!(table.total, 12_000);
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

#[test]
fn re_thinning_holds_growing_lines() {
    let lens: Vec<u64> = (0..6000).map(|i| 100 + i).collect();
    let (offsets, thins) = driven(&lens);
    assert!(thins >= 1, "{thins} thinnings");
    let table = offsets.lock();
    assert!(table.checkpoints.len() <= CHECKPOINTS);
    assert_eq!(table.total, 6000);
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

#[test]
fn lines_longer_than_the_spacing_become_checkpoints() {
    // After a thinning the spacing is hundreds of bytes. A run of lines
    // each longer than it ends checkpointed: each starts a whole long
    // line past any earlier checkpoint, whatever came before.
    let mut lens = vec![200u64; 1025];
    lens.extend(vec![5000u64; 5]);
    let (offsets, thins) = driven(&lens);
    assert!(thins >= 1, "{thins} thinnings");
    let table = offsets.lock();
    // Every long line after the first is checkpointed: each starts a
    // whole long line past any earlier checkpoint. The first long line
    // itself can land within the spacing past the previous checkpoint.
    for line in 1026..1030 {
        assert!(
            table.checkpoints.iter().any(|c| c.0 == line),
            "line {line} not checkpointed"
        );
    }
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

#[test]
fn a_giant_line_stays_a_bounded_gap() {
    // A single giant line need not be checkpointed itself — its start
    // can lie within the spacing past the previous checkpoint — but the
    // gap holding it stays within twice the spacing plus its length.
    let mut lens = vec![200u64; 3000];
    lens.push(1_500_000);
    lens.extend(vec![200u64; 3000]);
    let (offsets, thins) = driven(&lens);
    assert!(thins >= 1, "{thins} thinnings");
    let table = offsets.lock();
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

#[test]
fn one_long_line_stays_a_bounded_gap() {
    // A single long line among small ones: the halves hold whether or
    // not its start lands on a checkpoint.
    let mut lens = vec![200u64; 1500];
    lens.insert(700, 10_000);
    lens.extend(vec![200u64; 1500]);
    let (offsets, thins) = driven(&lens);
    assert!(thins >= 1, "{thins} thinnings");
    let table = offsets.lock();
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

#[test]
fn consecutive_long_lines_stay_bounded_gaps() {
    let mut lens = vec![200u64; 2000];
    lens.extend(vec![10_000u64; 20]);
    lens.extend(vec![200u64; 4000]);
    let (offsets, thins) = driven(&lens);
    assert!(thins >= 1, "{thins} thinnings");
    let table = offsets.lock();
    assert_gaps(&table.checkpoints, table.spacing, &lens);
    assert_spacing(table.spacing, table.end);
}

/// Envelope lines bulk-written to a log file: `payloads` in order, each
/// with its absolute `seq`. Returns the directory, the table built by
/// mirroring per-line pushes (no per-line fsync), the envelopes in order,
/// and each line's byte length.
fn bulked(
    name: &str,
    payloads: Vec<serde_json::Value>,
) -> (fakes::TempDir, Offsets, Vec<Envelope>, Vec<u64>) {
    let sessions = fakes::TempDir::new(&format!("log-offsets-bulk-{name}"));
    let path = sessions.path().join("events.jsonl");
    let mut bytes = Vec::new();
    let mut envelopes: Vec<Envelope> = Vec::new();
    let mut lens = Vec::new();
    for (seq, payload) in payloads.into_iter().enumerate() {
        let text = line(
            "step_started",
            "s_1",
            0,
            u64::try_from(seq).unwrap_or(u64::MAX),
            &payload,
        );
        lens.push(u64::try_from(text.len()).unwrap_or(u64::MAX));
        bytes.extend_from_slice(text.as_bytes());
        envelopes.push(serde_json::from_str(&text).unwrap());
    }
    std::fs::write(&path, &bytes).unwrap();
    let offsets = Offsets::new(path, Vec::new(), 0);
    for len in &lens {
        offsets.push(*len);
    }
    (sessions, offsets, envelopes, lens)
}

fn small_payloads(n: usize) -> Vec<serde_json::Value> {
    vec![serde_json::json!({}); n]
}

fn long_payload() -> serde_json::Value {
    serde_json::json!({"pad": "x".repeat(9900)})
}

/// The first resident gap holding at least two lines: reading inside
/// it forces the forward scan, since no checkpoint stands between a
/// gap's bounding lines.
fn roomy_gap(checkpoints: &[(u64, u64)]) -> (u64, u64) {
    for pair in checkpoints.windows(2) {
        if pair[1].0 >= pair[0].0.saturating_add(2) {
            return (pair[0].0, pair[1].0);
        }
    }
    panic!("no gap holds two lines");
}

/// The widest resident gap: its bounding lines and bytes, and the longest
/// line wholly inside it.
fn widest(checkpoints: &[(u64, u64)], lens: &[u64]) -> (u64, u64, u64, u64) {
    let mut best = (0, 0, 0, 0);
    for pair in checkpoints.windows(2) {
        let (a_line, a_byte) = pair[0];
        let (b_line, b_byte) = pair[1];
        let gap = b_byte.saturating_sub(a_byte);
        if gap > best.2 {
            let mut longest = 0;
            let mut line = a_line;
            while line < b_line {
                if let Ok(i) = usize::try_from(line) {
                    longest = longest.max(*lens.get(i).unwrap_or(&0));
                }
                line = line.saturating_add(1);
                if line == u64::MAX {
                    break;
                }
            }
            best = (a_line, b_line, gap, longest);
        }
    }
    best
}

/// The metered scan to `target` stays within the ruled bound: the spacing
/// term from the table's bytes, the gap's longest line, and one page.
fn assert_scan(offsets: &Offsets, target: u64, end: u64, longest: u64) {
    let window = offsets
        .resolve(target, 4, u64::MAX, u64::MAX)
        .unwrap()
        .unwrap();
    let bound = (2u64.max(end.saturating_mul(8) / 1024))
        .saturating_add(longest)
        .saturating_add(PAGE_BYTES);
    assert!(
        window.scanned <= bound,
        "target {target}: scanned {} over bound {bound}",
        window.scanned
    );
}

#[test]
fn evicted_reads_are_exact_across_the_widest_gap() {
    let mut payloads = vec![long_payload(); 1100];
    payloads.extend(small_payloads(600));
    let (_dir, offsets, envelopes, lens) = bulked("evicted", payloads);
    {
        let table = offsets.lock();
        assert!(
            table.spacing.trailing_zeros() >= 8,
            "spacing {}",
            table.spacing
        );
        assert_eq!(table.total, envelopes.len() as u64);
    }
    let (a_line, b_line, _gap, longest) = widest(&offsets.lock().checkpoints, &lens);
    let end = offsets.end();
    // The page starting just before the old sparse checkpoint at the
    // gap's end: every line before the failure boundary is exact, with
    // absolute seqs.
    let from = b_line.saturating_sub(2);
    let page = offsets.page(from, u64::MAX);
    let first = usize::try_from(from).unwrap_or(usize::MAX);
    assert_eq!(page.lines, envelopes[first..first + page.lines.len()]);
    assert_eq!(
        page.lines.first().map(|line| line.seq),
        envelopes.get(first).map(|line| line.seq)
    );
    let more = u64::try_from(first + page.lines.len()).unwrap_or(u64::MAX) < offsets.count();
    assert_eq!(matches!(page.after, After::More), more);
    // A range wholly inside the gap reads exactly.
    let at = a_line.saturating_add(1);
    let at_usize = usize::try_from(at).unwrap_or(usize::MAX);
    let got = offsets.range(at, 4).unwrap();
    assert_eq!(got, envelopes[at_usize..at_usize + got.len()]);
    // The first page reads exactly, through the metered scan.
    let first_page = offsets.page(0, u64::MAX);
    assert_eq!(first_page.lines, envelopes[..first_page.lines.len()]);
    // The worst scan stays within the ruled bound, and so does the first
    // page's sizing scan.
    assert_scan(&offsets, b_line.saturating_sub(1), end, longest);
    assert_scan(&offsets, 0, end, longest);
}

#[test]
fn evicted_reads_are_exact_over_uniform_long_lines() {
    let payloads = vec![long_payload(); 1500];
    let (_dir, offsets, envelopes, lens) = bulked("uniform-long", payloads);
    {
        let table = offsets.lock();
        assert!(
            table.spacing.trailing_zeros() >= 8,
            "spacing {}",
            table.spacing
        );
        assert_gaps(&table.checkpoints, table.spacing, &lens);
        assert_spacing(table.spacing, table.end);
    }
    let (a_line, b_line, _gap, longest) = widest(&offsets.lock().checkpoints, &lens);
    let end = offsets.end();
    let at = a_line.saturating_add(1);
    let at_usize = usize::try_from(at).unwrap_or(usize::MAX);
    let got = offsets.range(at, 4).unwrap();
    assert_eq!(got, envelopes[at_usize..at_usize + got.len()]);
    assert_scan(&offsets, b_line.saturating_sub(1), end, longest);
    assert_scan(&offsets, 0, end, longest);
    // Past-end reads stay empty: past the last line, and at `u64::MAX`.
    assert!(offsets.range(offsets.count(), 4).unwrap().is_empty());
    assert!(offsets.range(u64::MAX, 4).unwrap().is_empty());
    let page = offsets.page(u64::MAX, u64::MAX);
    assert!(page.lines.is_empty());
    assert!(matches!(page.after, After::Done));
}

#[test]
fn evicted_reads_are_exact_around_a_giant_line() {
    let mut payloads = small_payloads(1500);
    payloads.push(serde_json::json!({"pad": "x".repeat(1_500_000)}));
    payloads.extend(small_payloads(1500));
    let (_dir, offsets, envelopes, lens) = bulked("giant", payloads);
    {
        let table = offsets.lock();
        assert_gaps(&table.checkpoints, table.spacing, &lens);
        assert_spacing(table.spacing, table.end);
    }
    // A line longer than a page is a page of its own, even evicted.
    let page = offsets.page(1500, u64::MAX);
    assert_eq!(page.lines, envelopes[1500..1501]);
    assert!(matches!(page.after, After::More));
    assert_eq!(offsets.range(1501, 3).unwrap(), envelopes[1501..1504]);
    // The giant line dominates the ruled bound, so it holds vacuously
    // at this scale; the halves above carry the guarantee.
}

#[test]
fn evicted_reads_are_exact_around_one_long_line() {
    let mut payloads = small_payloads(1500);
    payloads.insert(700, long_payload());
    payloads.extend(small_payloads(1500));
    let (_dir, offsets, envelopes, lens) = bulked("one-long", payloads);
    {
        let table = offsets.lock();
        assert_gaps(&table.checkpoints, table.spacing, &lens);
        assert_spacing(table.spacing, table.end);
    }
    // A range starting inside a roomy gap (never on a checkpoint)
    // forces the forward scan, as does the worst-case metered read below.
    let (_a, b_line, _gap, longest) = widest(&offsets.lock().checkpoints, &lens);
    let (r_line, _r_end) = roomy_gap(&offsets.lock().checkpoints);
    let end = offsets.end();
    let at = r_line.saturating_add(1);
    let at_usize = usize::try_from(at).unwrap_or(usize::MAX);
    let got = offsets.range(at, 2).unwrap();
    assert_eq!(got, envelopes[at_usize..at_usize + got.len()]);
    assert_eq!(offsets.range(0, 2).unwrap(), envelopes[0..2]);
    assert_scan(&offsets, b_line.saturating_sub(1), end, longest);
    // The file is smaller than the page slack, so the ruled bound holds
    // vacuously here; the halves above carry the guarantee.
}

#[test]
fn evicted_reads_are_exact_across_consecutive_long_lines() {
    let mut payloads = small_payloads(1500);
    payloads.extend(vec![long_payload(); 20]);
    payloads.extend(small_payloads(1500));
    let (_dir, offsets, envelopes, lens) = bulked("consec-long", payloads);
    {
        let table = offsets.lock();
        assert_gaps(&table.checkpoints, table.spacing, &lens);
        assert_spacing(table.spacing, table.end);
    }
    let (_a, b_line, _gap, longest) = widest(&offsets.lock().checkpoints, &lens);
    let end = offsets.end();
    // A range spanning the whole long run reads exactly.
    assert_eq!(offsets.range(1500, 20).unwrap(), envelopes[1500..1520]);
    // The file is smaller than the page slack, so the ruled bound holds
    // vacuously here; the halves above carry the guarantee.
    assert_scan(&offsets, b_line.saturating_sub(1), end, longest);
}

#[test]
fn evicted_reads_are_exact_over_alternating_lines() {
    let mut payloads = Vec::new();
    for _ in 0..1000 {
        payloads.push(serde_json::json!({}));
        payloads.push(long_payload());
    }
    let (_dir, offsets, envelopes, lens) = bulked("alternating", payloads);
    {
        let table = offsets.lock();
        assert_gaps(&table.checkpoints, table.spacing, &lens);
        assert_spacing(table.spacing, table.end);
    }
    // Inside a roomy gap, never on a checkpoint: forces the scan.
    let (_a, b_line, _gap, longest) = widest(&offsets.lock().checkpoints, &lens);
    let (r_line, _r_end) = roomy_gap(&offsets.lock().checkpoints);
    let end = offsets.end();
    let at = r_line.saturating_add(1);
    let at_usize = usize::try_from(at).unwrap_or(usize::MAX);
    let got = offsets.range(at, 4).unwrap();
    assert_eq!(got, envelopes[at_usize..at_usize + got.len()]);
    assert_scan(&offsets, b_line.saturating_sub(1), end, longest);
    assert_scan(&offsets, 0, end, longest);
}

#[test]
fn evicted_reads_are_exact_over_growing_lines() {
    let payloads: Vec<serde_json::Value> = (0..3000)
        .map(|i| serde_json::json!({"pad": "x".repeat(i % 3000)}))
        .collect();
    let (_dir, offsets, envelopes, lens) = bulked("growing", payloads);
    {
        let table = offsets.lock();
        assert_gaps(&table.checkpoints, table.spacing, &lens);
        assert_spacing(table.spacing, table.end);
    }
    let (_a, b_line, _gap, longest) = widest(&offsets.lock().checkpoints, &lens);
    let end = offsets.end();
    assert_eq!(offsets.range(2000, 4).unwrap(), envelopes[2000..2004]);
    // A deep line's scan crosses the whole prefix when evicted: still
    // within the ruled bound.
    assert_scan(&offsets, 2900, end, longest);
    assert_scan(&offsets, b_line.saturating_sub(1), end, longest);
}

#[test]
fn a_corrupt_line_in_an_evicted_gap_names_its_absolute_line() {
    let mut payloads = vec![long_payload(); 1100];
    payloads.extend(small_payloads(600));
    let (dir, offsets, envelopes, lens) = bulked("evicted-bad", payloads);
    let (a_line, _b, _gap, _longest) = widest(&offsets.lock().checkpoints, &lens);
    let bad = usize::try_from(a_line.saturating_add(1)).unwrap_or(usize::MAX);
    corrupt(dir.path(), bad);
    // A page reaching the corrupt line keeps every line before it, then
    // the failure with its absolute one-based number. It starts just
    // before the line: a page holds at most one megabyte, so a page from
    // zero would end before a far line with more, not the failure.
    let from = bad.saturating_sub(2);
    let page = offsets.page(u64::try_from(from).unwrap_or(u64::MAX), u64::MAX);
    assert_eq!(page.lines, envelopes[from..bad]);
    let After::Failed(error) = page.after else {
        panic!("a page over an unparseable line fails");
    };
    assert!(
        error.to_string().contains(&format!("line {}", bad + 1)),
        "{error}"
    );
    // A range over it is an error with no lines.
    let err = offsets
        .range(u64::try_from(from).unwrap_or(u64::MAX), bad + 1)
        .unwrap_err();
    assert!(
        err.to_string().contains(&format!("line {}", bad + 1)),
        "{err}"
    );
}

#[test]
fn a_cut_in_an_evicted_gap_keeps_the_whole_lines_before_it() {
    let mut payloads = vec![long_payload(); 1100];
    payloads.extend(small_payloads(600));
    let (dir, offsets, envelopes, lens) = bulked("evicted-cut", payloads);
    let (a_line, _b_line, gap, _longest) = widest(&offsets.lock().checkpoints, &lens);
    // Cut halfway through the widest gap: only whole lines come before
    // the failure, and the failure is the short read.
    let a_byte = offsets
        .lock()
        .checkpoints
        .iter()
        .find(|c| c.0 == a_line)
        .map(|c| c.1)
        .unwrap_or(0);
    let _cut = cut(
        dir.path(),
        usize::try_from(a_byte + gap / 2).unwrap_or(usize::MAX),
    );
    // The page starts just before the cut for the same megabyte reason
    // as above: it keeps the whole lines before the cut, then the short
    // read failure.
    let from = usize::try_from(a_line.saturating_sub(2)).unwrap_or(0);
    let cut_at = a_byte + gap / 2;
    let page = offsets.page(u64::try_from(from).unwrap_or(u64::MAX), u64::MAX);
    // Every returned line ends at or before the cut.
    let mut start = 0;
    for (i, len) in lens.iter().enumerate() {
        let stop = start + len;
        if i >= from {
            if stop > cut_at {
                assert_eq!(page.lines, envelopes[from..i]);
                break;
            }
        } else {
            assert!(stop <= cut_at, "cut lands before the page");
        }
        start = stop;
    }
    let After::Failed(Error::Io { source, .. }) = page.after else {
        panic!("a page over a log cut short fails");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::UnexpectedEof);
    let err = offsets.range(0, 10_000).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
}

#[test]
fn scan_with_a_small_limit_reads_only_that_many_lines() {
    let mut payloads = vec![long_payload(); 1100];
    payloads.extend(small_payloads(600));
    let (dir, _offsets, envelopes, _lens) = bulked("evicted-limit", payloads);
    let session = dir.path();
    let scanned = Offsets::scan(session, 5, |_| Ok(())).unwrap();
    assert_eq!(scanned.count(), 5);
    assert_eq!(scanned.range(0, 100).unwrap(), envelopes[..5]);
}
