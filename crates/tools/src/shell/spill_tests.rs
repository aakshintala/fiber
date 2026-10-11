//! The spill past 1 MiB: the file, its two ends, and every failure on the
//! way there.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::fs::File;
use std::io::{self, Write};
use std::sync::Arc;

use contract::tool::Bound;
use fakes::TempDir;

use super::{
    Config, Ring, SPILL_AT, Spill, decode_prefix, decode_suffix, random_name, still_there,
    trim_split_start,
};

fn config(dir: &TempDir, start: usize, end: usize) -> Config {
    Config {
        dir: dir.path().to_path_buf(),
        bound: Bound { start, end },
        wrap: None,
        name: Some("s_test.txt".to_owned()),
    }
}

/// Asserts two byte strings are equal by length and first differing offset,
/// without printing either whole on failure.
fn assert_bytes_eq(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len(), "bytes length");
    let offset = actual
        .iter()
        .zip(expected.iter())
        .position(|(left, right)| left != right);
    assert_eq!(offset, None, "first differing byte offset");
}

/// The first and last windows of `bytes`, for values too large to assert whole.
fn ends(bytes: &[u8]) -> (&[u8], &[u8]) {
    let head = bytes.get(..100).unwrap_or(bytes);
    let tail = bytes
        .get(bytes.len().saturating_sub(100)..)
        .unwrap_or(bytes);
    (head, tail)
}

#[test]
fn a_crossing_chunk_spills_from_the_first_byte() {
    let dir = TempDir::new("fiber-spill-first-byte");
    let mut buffer = vec![b'a'; SPILL_AT];
    buffer[8191] = b'H';
    buffer[8192] = b'T';
    buffer[SPILL_AT - 8193] = b'R';
    buffer[SPILL_AT - 1] = b'Z';
    let mut spill = Spill::start(config(&dir, 8192, 8192), buffer);
    spill.push(b"q");
    let path = dir.path().join("s_test.txt");
    let file = std::fs::read(&path).unwrap();
    // Every byte the reader saw, in order: the buffer and the chunk.
    assert_eq!(file.len(), SPILL_AT + 1);
    let (head, tail) = ends(&file);
    assert_eq!(head, &[b'a'; 100][..]);
    assert_eq!(&tail[..98], &[b'a'; 98][..]);
    assert_eq!(tail[98], b'Z');
    assert_eq!(tail[99], b'q', "the chunk is last");
    assert_eq!(file[8191], b'H');
    assert_eq!(file[8192], b'T');
    assert_eq!(file[SPILL_AT - 8193], b'R');
    assert_eq!(file[SPILL_AT - 1], b'Z');
    assert_eq!(file[SPILL_AT], b'q');
}

#[test]
fn close_holds_only_the_two_ends_and_accounts_every_byte() {
    let dir = TempDir::new("fiber-spill-ends");
    let spill = Spill::start(config(&dir, 8192, 8192), vec![b'a'; SPILL_AT]);
    let spilled = spill.close(None, "Exit code 0.");
    let file = std::fs::read(dir.path().join("s_test.txt")).unwrap();
    let kept = spilled.file.as_ref().expect("the file is kept");
    assert_eq!(kept.size, file.len() as u64);
    // The closing lines end the file and the tail.
    assert!(spilled.tail.ends_with("\nExit code 0.\n"));
    assert_bytes_eq(
        file[file.len() - spilled.tail.len()..].to_vec().as_slice(),
        spilled.tail.as_bytes(),
    );
    // The head is exactly the first 8,192 bytes: by length and windows.
    assert_eq!(spilled.head.len(), 8192);
    assert_eq!(&spilled.head.as_bytes()[..100], &[b'a'; 100][..]);
    assert_eq!(&spilled.head.as_bytes()[8092..], &[b'a'; 100][..]);
    assert_bytes_eq(&file[..8192], spilled.head.as_bytes());
    // Head, then the removed middle, then tail reconstruct the file.
    assert_eq!(
        spilled.removed,
        file.len() as u64 - 8192 - spilled.tail.len() as u64
    );
    assert_eq!(spilled.removed, kept.size - 16_384);
    let middle = file
        .get(8192..file.len() - spilled.tail.len())
        .unwrap_or_default();
    assert_eq!(middle.len() as u64, spilled.removed);
    assert_eq!(middle[..100], [b'a'; 100]);
}

#[test]
fn a_ring_fed_in_chunks_that_straddle_its_size_keeps_order() {
    let dir = TempDir::new("fiber-spill-ring");
    let mut spill = Spill::start(config(&dir, 4, 10), b"".to_vec());
    spill.push(b"0123456");
    spill.push(b"789");
    spill.push(b"abcdefghij");
    assert_eq!(spill.ring.bytes, b"abcdefghij".to_owned());
    assert_eq!(spill.head, b"0123".to_owned());
    assert_eq!(spill.total, 7 + 3 + 10);
}

#[test]
fn a_head_ending_inside_a_character_ends_before_it() {
    let dir = TempDir::new("fiber-spill-head-char");
    // 8,191 ASCII bytes, then a 3-byte character straddling 8,192.
    let mut buffer = vec![b'a'; 8191];
    buffer.extend_from_slice("é".as_bytes());
    buffer.extend_from_slice(&[b'b'; 100]);
    let spill = Spill::start(config(&dir, 8192, 8192), buffer);
    let spilled = spill.close(None, "Exit code 0.");
    assert_eq!(spilled.head.len(), 8191);
    assert!(spilled.head.is_ascii());
    // The partial character's bytes are in neither end.
    let kept = spilled.file.as_ref().expect("the file is kept");
    assert_eq!(
        spilled.removed,
        kept.size - 8191 - spilled.tail.len() as u64
    );
    let file = std::fs::read(dir.path().join("s_test.txt")).unwrap();
    assert_eq!(&file[..8191], spilled.head.as_bytes());
    assert_eq!(&file[8191..8193], "é".as_bytes());
}

#[test]
fn a_ring_starting_on_continuation_bytes_drops_at_most_three() {
    // Five continuation bytes, then ASCII: the ring keeps the last two of
    // the five, decoding them as U+FFFD each, and the ASCII after them.
    let bytes = [vec![0x80; 5], b"hello".to_vec()].concat();
    assert_eq!(
        trim_split_start(&bytes),
        &[0x80, 0x80, b'h', b'e', b'l', b'l', b'o'][..]
    );
    for n in 0..=4_usize {
        let bytes = [vec![0x80; n], b"hello".to_vec()].concat();
        let trimmed = trim_split_start(&bytes);
        let skipped = n.min(3);
        assert_eq!(trimmed.len(), bytes.len() - skipped, "n = {n}");
    }
    let dir = TempDir::new("fiber-spill-ring-char");
    let mut buffer = vec![b'a'; 8192];
    buffer.extend_from_slice(&[0x80; 5]);
    buffer.extend_from_slice(b"hello");
    let spill = Spill::start(config(&dir, 8192, 8192), buffer);
    let spilled = spill.close(None, "Exit code 0.");
    assert!(spilled.tail.contains("hello"));
    assert_eq!(spilled.tail.chars().filter(|c| *c == '\u{fffd}').count(), 5);
}

#[test]
fn invalid_bytes_at_both_ends_count_three_each() {
    let dir = TempDir::new("fiber-spill-invalid");
    let buffer = vec![0xFF; 20_000];
    let spill = Spill::start(config(&dir, 8192, 8192), buffer);
    let spilled = spill.close(None, "Exit code 0.");
    // 8,192 raw bytes of 0xFF decode to 2,730 U+FFFD: 8,190 bytes.
    assert_eq!(spilled.head.chars().count(), 2730);
    assert_eq!(spilled.head.len(), 8190);
    assert!(spilled.head.chars().all(|c| c == '\u{fffd}'));
    assert!(spilled.tail.len() <= 8192);
    assert!(spilled.tail.ends_with("\nExit code 0.\n"));
    let kept = spilled.file.as_ref().expect("the file is kept");
    assert_eq!(spilled.removed, kept.size - 2730 - tail_raw(&spilled.tail));
}

/// The raw bytes behind `tail`: its length minus 2 per U+FFFD, since each
/// one stands for one raw byte but reads as three.
fn tail_raw(tail: &str) -> u64 {
    let replacements = tail.chars().filter(|c| *c == '\u{fffd}').count() as u64;
    tail.len() as u64 - 2 * replacements
}

#[test]
fn overlapping_ends_join_to_the_whole_with_nothing_removed() {
    let dir = TempDir::new("fiber-spill-overlap");
    let buffer = vec![b'a'; SPILL_AT + 1];
    let spill = Spill::start(config(&dir, 700_000, 700_000), buffer);
    let spilled = spill.close(None, "Exit code 0.");
    let path = dir.path().join("s_test.txt");
    assert!(path.is_file(), "the file exists");
    let file = std::fs::read(&path).unwrap();
    assert_eq!(file.len(), SPILL_AT + 1 + 14);
    assert_eq!(spilled.removed, 0);
    // Head and tail join to the whole lossy text: by length and windows.
    assert_eq!(spilled.head.len() + spilled.tail.len(), file.len());
    assert_bytes_eq(
        [spilled.head.as_bytes(), spilled.tail.as_bytes()]
            .concat()
            .as_slice(),
        &file,
    );
    let kept = spilled.file.as_ref().expect("the file is kept");
    assert_eq!(kept.size, file.len() as u64);
}

#[test]
fn overlapping_ends_past_the_cap_count_the_rest_removed() {
    let dir = TempDir::new("fiber-spill-overlap-cap");
    let buffer = vec![b'b'; 2_000_000];
    let spill = Spill::start(config(&dir, 700_000, 700_000), buffer);
    let spilled = spill.close(None, "Exit code 0.");
    assert_eq!(spilled.head.len(), 700_000);
    assert_eq!(spilled.tail.len(), 700_000);
    // 2,000,000 bytes plus the 14 closing bytes, minus the two ends.
    assert_eq!(spilled.removed, 600_000 + 14);
}

#[test]
fn a_create_failure_names_the_path_and_leaves_no_file() {
    let dir = TempDir::new("fiber-spill-create");
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    let before: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    let spill = Spill::start(
        Config {
            dir: file.clone(),
            bound: Bound { start: 8, end: 8 },
            wrap: None,
            name: Some("s_test.txt".to_owned()),
        },
        b"hi".to_vec(),
    );
    let spilled = spill.close(None, "Exit code 0.");
    let Err(failed) = spilled.file else {
        panic!("a spill into a file is kept");
    };
    assert!(failed.message.contains("s_test.txt"), "{}", failed.message);
    assert_eq!(failed.left, None);
    let after: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(before.len(), after.len(), "no file was created");
    // The command runs on with only the ends held: the closing lines join
    // the head once closed.
    assert_eq!(spilled.head, "hi\nExit ");
}

#[test]
fn a_name_collision_leaves_the_existing_file_untouched() {
    let dir = TempDir::new("fiber-spill-collision");
    std::fs::write(dir.path().join("s_test.txt"), b"kept").unwrap();
    let spill = Spill::start(config(&dir, 8, 8), b"hi".to_vec());
    let spilled = spill.close(None, "Exit code 0.");
    assert!(spilled.file.is_err());
    assert_eq!(
        std::fs::read(dir.path().join("s_test.txt")).unwrap(),
        b"kept"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn a_non_utf8_directory_is_refused_before_any_file() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("fiber-spill-utf8");
    let bad = dir.path().join(OsStr::from_bytes(b"bad-\xff-dir"));
    std::fs::create_dir(&bad).unwrap();
    let spill = Spill::start(
        Config {
            dir: bad.clone(),
            bound: Bound { start: 8, end: 8 },
            wrap: None,
            name: Some("s_test.txt".to_owned()),
        },
        b"hi".to_vec(),
    );
    let spilled = spill.close(None, "Exit code 0.");
    let Err(failed) = spilled.file else {
        panic!("a spill under a non-UTF-8 path is kept");
    };
    assert!(
        failed.message.contains("could not save"),
        "{}",
        failed.message
    );
    assert_eq!(failed.left, None);
    assert!(
        std::fs::read_dir(&bad).unwrap().next().is_none(),
        "no file was created"
    );
}

/// A writer that fails after `writes` successful writes.
struct FailAfter {
    file: File,
    writes: std::cell::Cell<usize>,
}

impl Write for FailAfter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.writes.get() == 0 {
            return Err(io::Error::other("boom"));
        }
        self.writes.set(self.writes.get() - 1);
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn failing(writes: usize) -> super::Wrap {
    Arc::new(move |file| {
        let failing: Box<dyn Write + Send> = Box::new(FailAfter {
            file,
            writes: std::cell::Cell::new(writes),
        });
        failing
    })
}

#[test]
fn a_write_failure_removes_the_file_it_created() {
    let dir = TempDir::new("fiber-spill-write-fail");
    let spill = Spill::start(
        Config {
            dir: dir.path().to_path_buf(),
            bound: Bound { start: 8, end: 8 },
            wrap: Some(failing(0)),
            name: Some("s_test.txt".to_owned()),
        },
        b"hi".to_vec(),
    );
    let spilled = spill.close(None, "Exit code 0.");
    let Err(failed) = spilled.file else {
        panic!("a failed spill is kept");
    };
    assert!(failed.message.contains("s_test.txt"), "{}", failed.message);
    assert!(failed.message.contains("boom"), "{}", failed.message);
    assert_eq!(failed.left, None);
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "the partial file was removed"
    );
    // The head and the ring are still held: the closing lines join the
    // head once closed.
    assert_eq!(spilled.head, "hi\nExit ");
}

#[test]
fn a_removal_failure_leaves_the_file_as_left() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("fiber-spill-remove-fail");
    let spill = Spill::start(
        Config {
            dir: dir.path().to_path_buf(),
            bound: Bound { start: 8, end: 8 },
            wrap: Some(failing(1)),
            name: Some("s_test.txt".to_owned()),
        },
        b"hi".to_vec(),
    );
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut spill = spill;
    spill.push(b"bye");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let spilled = spill.close(None, "Exit code 0.");
    let Err(failed) = spilled.file else {
        panic!("a failed spill is kept");
    };
    let left = failed.left.expect("the surviving file");
    assert_eq!(left, dir.path().join("s_test.txt"));
    assert!(failed.message.contains("boom"), "{}", failed.message);
    assert!(
        failed.message.contains("could not be removed")
            || failed.message.contains("It could not be removed"),
        "{}",
        failed.message
    );
}

#[test]
fn a_closing_lines_failure_removes_the_file_it_created() {
    let dir = TempDir::new("fiber-spill-close-fail");
    let spill = Spill::start(
        Config {
            dir: dir.path().to_path_buf(),
            bound: Bound { start: 8, end: 8 },
            wrap: Some(failing(1)),
            name: Some("s_test.txt".to_owned()),
        },
        b"hi".to_vec(),
    );
    // The buffer's write is the one success; the closing lines fail.
    let spilled = spill.close(None, "Exit code 0.");
    assert!(spilled.file.is_err());
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "the partial file was removed"
    );
}

#[test]
fn rename_into_a_job_file_keeps_the_handle_and_leaves_no_spill() {
    let dir = TempDir::new("fiber-spill-rename");
    let spill = Spill::start(config(&dir, 8, 8), b"0123456789abcdef".to_vec());
    let job = dir.path().join("j_1.log");
    let registry = File::create(&job).unwrap();
    let (mut sink, line) = spill.into_job(&job, registry);
    assert_eq!(line, None);
    sink.write_all(b"later").unwrap();
    drop(sink);
    assert_eq!(std::fs::read(&job).unwrap(), b"0123456789abcdeflater");
    assert!(!dir.path().join("s_test.txt").exists());
}

#[test]
fn a_failed_rename_copies_into_the_registry_file_and_removes_the_spill() {
    let dir = TempDir::new("fiber-spill-copy");
    let spill = Spill::start(config(&dir, 8, 8), b"0123456789abcdef".to_vec());
    let job = dir.path().join("missing").join("j_1.log");
    let registry_path = dir.path().join("registry.log");
    let registry = File::create(&registry_path).unwrap();
    let (mut sink, line) = spill.into_job(&job, registry);
    assert_eq!(line, None);
    sink.write_all(b"later").unwrap();
    drop(sink);
    assert_eq!(
        std::fs::read(&registry_path).unwrap(),
        b"0123456789abcdeflater"
    );
    assert!(!dir.path().join("s_test.txt").exists());
}

#[test]
fn a_failed_copy_keeps_the_spilled_file_and_names_it() {
    let dir = TempDir::new("fiber-spill-copy-fail");
    let spill = Spill::start(config(&dir, 8, 8), b"0123456789abcdef".to_vec());
    // Nowhere to rename to, and a read-only registry no copy can fill.
    let job = dir.path().join("missing").join("j_1.log");
    let readonly = dir.path().join("readonly.log");
    std::fs::write(&readonly, b"").unwrap();
    let registry = File::open(&readonly).unwrap();
    let (_sink, line) = spill.into_job(&job, registry);
    let line = line.expect("the receipt names the file");
    assert!(line.contains("s_test.txt"), "{line}");
    assert_eq!(
        std::fs::read(dir.path().join("s_test.txt")).unwrap(),
        b"0123456789abcdef"
    );
}

#[test]
fn a_failed_spill_moved_to_a_job_writes_head_then_ring() {
    let dir = TempDir::new("fiber-spill-failed-move");
    let spill = Spill::start(
        Config {
            dir: dir.path().join("not-a-dir"),
            bound: Bound { start: 4, end: 4 },
            wrap: None,
            name: Some("s_test.txt".to_owned()),
        },
        b"0123456789".to_vec(),
    );
    let job = dir.path().join("j_1.log");
    let registry = File::create(&job).unwrap();
    let (sink, line) = spill.into_job(&job, registry);
    assert_eq!(line, None);
    drop(sink);
    assert_eq!(std::fs::read(&job).unwrap(), b"01236789");
}

#[test]
fn a_left_file_is_retried_at_the_move() {
    use std::os::unix::fs::PermissionsExt;
    fn failing_spill(dir: &TempDir) -> Spill {
        Spill::start(
            Config {
                dir: dir.path().to_path_buf(),
                bound: Bound { start: 8, end: 8 },
                wrap: Some(failing(1)),
                name: Some("s_test.txt".to_owned()),
            },
            b"hi".to_vec(),
        )
    }
    fn readonly(dir: &TempDir) {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    fn writable(dir: &TempDir) {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // While the directory stays read-only the surviving file is named.
    let dir = TempDir::new("fiber-spill-left-readonly");
    let job = dir.path().join("j_1.log");
    File::create(&job).unwrap();
    let mut spill = failing_spill(&dir);
    readonly(&dir);
    spill.push(b"bye");
    let (sink, line) = spill.into_job(
        &job,
        std::fs::OpenOptions::new().write(true).open(&job).unwrap(),
    );
    drop(sink);
    writable(&dir);
    let line = line.expect("the surviving file is named");
    assert!(line.contains("s_test.txt"), "{line}");
    assert!(dir.path().join("s_test.txt").is_file());
    // Writable again before the move, the retry removes it with no line.
    let dir = TempDir::new("fiber-spill-left-writable");
    let job = dir.path().join("j_1.log");
    File::create(&job).unwrap();
    let mut spill = failing_spill(&dir);
    readonly(&dir);
    spill.push(b"bye");
    writable(&dir);
    let (sink, line) = spill.into_job(
        &job,
        std::fs::OpenOptions::new().write(true).open(&job).unwrap(),
    );
    assert_eq!(line, None);
    drop(sink);
    assert!(!dir.path().join("s_test.txt").exists());
}

#[test]
fn close_of_an_empty_spill_adds_no_leading_newline() {
    let dir = TempDir::new("fiber-spill-empty");
    let spill = Spill::start(config(&dir, 8192, 8192), Vec::new());
    let spilled = spill.close(None, "Exit code 0.");
    let file = std::fs::read(dir.path().join("s_test.txt")).unwrap();
    assert_bytes_eq(&file, b"Exit code 0.\n");
    assert_eq!(spilled.head, "Exit code 0.\n");
    assert_eq!(spilled.tail, "");
    assert_eq!(spilled.removed, 0);
}

#[test]
fn ends_that_meet_exactly_count_a_split_character_once() {
    let dir = TempDir::new("fiber-spill-meet");
    // 16,370 output bytes plus the 14 closing bytes meet 8,192 + 8,192
    // exactly. A four-byte character straddles the head's end, so the
    // ring starts on its last byte: the head holds the replacement
    // character for its part, and the ring drops that byte.
    let mut buffer = vec![b'a'; 16_370];
    buffer[8189] = 0xF0;
    buffer[8190] = 0x9D;
    buffer[8191] = 0x84;
    buffer[8192] = 0x9E;
    let spill = Spill::start(config(&dir, 8192, 8192), buffer);
    let spilled = spill.close(None, "Exit code 0.");
    let file = std::fs::read(dir.path().join("s_test.txt")).unwrap();
    assert_eq!(file.len(), 16_384);
    assert_eq!(spilled.head.len(), 8192);
    assert_eq!(&spilled.head.as_bytes()[..100], &[b'a'; 100][..]);
    assert!(spilled.head.ends_with('\u{fffd}'));
    assert_eq!(spilled.tail.len(), 8191);
    assert!(spilled.tail.ends_with("\nExit code 0.\n"));
    assert_eq!(spilled.removed, 1);
}

#[test]
fn decode_prefix_stops_when_a_valid_run_is_cut_short() {
    let bytes = [b"ab".to_vec(), vec![0xF0, 0x9D, 0x84, 0x9E, 0xFF]].concat();
    let (text, used) = decode_prefix(&bytes, 5);
    assert_eq!(text, "ab");
    assert_eq!(used, 2);
}

#[test]
fn decode_prefix_counts_an_invalid_byte_as_three() {
    let (text, used) = decode_prefix(b"ab\xFFcd", 6);
    assert_eq!(text, "ab\u{fffd}c");
    assert_eq!(used, 4);
    let (text, used) = decode_prefix(b"ab\xFFcd", 5);
    assert_eq!(text, "ab\u{fffd}");
    assert_eq!(used, 3);
    let (text, used) = decode_prefix("éé".as_bytes(), 3);
    assert_eq!(text, "é");
    assert_eq!(used, 2);
    let (text, used) = decode_prefix(b"abc", 0);
    assert_eq!(text, "");
    assert_eq!(used, 0);
}

#[test]
fn decode_suffix_drops_whole_units_until_it_fits() {
    let (text, used) = decode_suffix(b"abcdef", 4);
    assert_eq!(text, "cdef");
    assert_eq!(used, 4);
    let (text, used) = decode_suffix("éé".as_bytes(), 3);
    assert_eq!(text, "é");
    assert_eq!(used, 2);
    let (text, used) = decode_suffix(b"a\xFFb", 4);
    assert_eq!(text, "\u{fffd}b");
    assert_eq!(used, 2);
    let (text, used) = decode_suffix(b"abc", 0);
    assert_eq!(text, "");
    assert_eq!(used, 0);
}

#[test]
fn trim_split_start_keeps_further_continuation_bytes() {
    assert_eq!(trim_split_start(b"abc"), b"abc");
    assert_eq!(
        trim_split_start(&[0x80, 0x80, 0x80, 0x80, b'a']),
        &[0x80, b'a'][..]
    );
    assert_eq!(trim_split_start(&[0xC3]), &[0xC3][..]);
    assert_eq!(trim_split_start(b""), b"");
}

#[test]
fn random_names_are_spill_files() {
    let first = random_name();
    assert!(first.starts_with("s_"));
    assert!(first.ends_with(".txt"));
    assert_eq!(first.len(), "s_".len() + 16 + ".txt".len());
    assert!(first[2..18].chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(first, random_name());
}

#[test]
fn the_receipt_line_names_the_raw_file() {
    let dir = TempDir::new("fiber-spill-line");
    let path = dir.path().join("s_x.txt");
    assert_eq!(
        still_there(&path),
        format!("Output before the move is also in {}.", path.display())
    );
}

#[test]
fn a_ring_at_its_cap_boundaries() {
    let mut ring = Ring {
        bytes: Vec::new(),
        cap: 10,
    };
    ring.push(b"123456789");
    assert_eq!(ring.bytes, b"123456789");
    ring.push(b"0");
    assert_eq!(ring.bytes, b"1234567890");
    ring.push(b"a");
    assert_eq!(ring.bytes, b"234567890a");
}
