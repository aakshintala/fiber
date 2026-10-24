//! Tests for `artifact_page` paging: the line and byte caps at, below
//! and above each edge, the character boundary shared by the cut and the
//! offset check, malformed bytes, and growth and shrinkage under the read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::*;

/// One page is at most this many lines and bytes.
const LINES_USIZE: usize = 64;
const BYTES_USIZE: usize = 64 * 1024;

struct Temp {
    dir: PathBuf,
    _held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("doors-artifact");
        let dir = held.path().to_path_buf();
        Self { dir, _held: held }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let file = self.dir.join(name);
        fs::write(&file, bytes).unwrap();
        file
    }

    fn open(&self, name: &str) -> File {
        File::open(self.dir.join(name)).unwrap()
    }
}

fn staged(bytes: &[u8]) -> (Temp, PathBuf) {
    let temp = Temp::new();
    let path = temp.write("a.txt", bytes);
    (temp, path)
}

fn page_at_path(path: &Path, offset: u64) -> Result<Page, PageError> {
    page(File::open(path).unwrap(), offset)
}

/// Lines of `width` bytes each, `count` of them, each with its newline.
fn lined(count: usize, width: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..count {
        bytes.extend(std::iter::repeat_n(b'a', width - 1));
        bytes.push(b'\n');
    }
    bytes
}

#[test]
fn offset_zero_of_an_empty_file_is_an_empty_page() {
    let (_temp, path) = staged(b"");
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.text, "");
    assert_eq!(found.next, None);
    assert_eq!(found.size, 0);
}

#[test]
fn offset_at_the_end_is_an_empty_page() {
    let (_temp, path) = staged(b"hello\n");
    let found = page_at_path(&path, 6).unwrap();
    assert_eq!(found.text, "");
    assert_eq!(found.next, None);
    assert_eq!(found.size, 6);
}

#[test]
fn offset_past_the_end_is_past() {
    let (_temp, path) = staged(b"hello\n");
    for offset in [7, 8, u64::MAX] {
        assert!(
            matches!(page_at_path(&path, offset), Err(PageError::Past)),
            "{offset}"
        );
    }
}

#[test]
fn an_offset_inside_a_character_is_inside_and_a_lead_answers() {
    // A 2-byte, a 3-byte and a 4-byte character, each alone in a file.
    for bytes in [b"\xc3\xa9".as_slice(), b"\xe2\x82\xac", b"\xf0\x9f\x98\x80"] {
        let (_temp, path) = staged(bytes);
        let size = u64::try_from(bytes.len()).unwrap();
        for offset in 1..size {
            assert!(
                matches!(page_at_path(&path, offset), Err(PageError::Inside)),
                "{bytes:?} at {offset}"
            );
        }
        let found = page_at_path(&path, 0).unwrap();
        assert_eq!(found.next, None);
        assert_eq!(found.size, size);
        assert_eq!(found.text, String::from_utf8_lossy(bytes));
    }
}

#[test]
fn malformed_bytes_are_not_inside_a_character() {
    // A lead followed by bytes that never complete it.
    for bytes in [b"\xe2\x80\x41".as_slice(), b"\xe0\x80\x80".as_slice()] {
        let (_temp, path) = staged(bytes);
        let found = page_at_path(&path, 1).unwrap();
        assert_eq!(found.size, 3);
        assert_eq!(found.text, String::from_utf8_lossy(bytes.get(1..).unwrap()));
    }
    // A valid character refused at its interior byte.
    let (_temp, path) = staged(b"\xe2\x82\xac");
    assert!(matches!(page_at_path(&path, 1), Err(PageError::Inside)));
    assert!(matches!(page_at_path(&path, 2), Err(PageError::Inside)));
    // A sequence truncated at the end of the file: the end is not a
    // boundary to reject.
    let (_temp, path) = staged(b"\xe2\x80");
    let found = page_at_path(&path, 1).unwrap();
    assert_eq!(found.next, None);
    assert_eq!(found.size, 2);
}

#[test]
fn a_stray_continuation_run_is_not_inside() {
    let bytes = [0x80u8; 5];
    let (_temp, path) = staged(&bytes);
    for offset in 0..=5 {
        let found = page_at_path(&path, offset).unwrap();
        assert_eq!(found.size, 5, "{offset}");
    }
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.text, "�����");
    assert_eq!(found.next, None);
}

#[test]
fn sixty_three_sixty_four_and_sixty_five_lines() {
    for (count, next) in [(63, None), (64, None), (65, Some(128))] {
        let (_temp, path) = staged(&lined(count, 2));
        let found = page_at_path(&path, 0).unwrap();
        assert_eq!(found.next, next, "{count} lines");
        let taken = next.map_or(count, |_| 64);
        assert_eq!(found.text, "a\n".repeat(taken));
    }
}

#[test]
fn a_line_at_below_and_above_the_byte_cap() {
    // One line of 65535, 65536 and 65537 `a` with no trailing newline:
    // the first two answer whole, the last is cut at the cap.
    for (width, next) in [(65535, None), (65536, None), (65537, Some(65536))] {
        let bytes = vec![b'a'; width];
        let (_temp, path) = staged(&bytes);
        let found = page_at_path(&path, 0).unwrap();
        assert_eq!(found.next, next, "{width}");
        assert_eq!(found.size, u64::try_from(width).unwrap());
        let taken = next.map_or(width, |at| usize::try_from(at).unwrap());
        assert_eq!(found.text.len(), taken, "{width}");
    }
}

#[test]
fn sixty_four_lines_of_1024_bytes_are_one_page() {
    let bytes = lined(LINES_USIZE, 1024);
    assert_eq!(bytes.len(), BYTES_USIZE);
    let (_temp, path) = staged(&bytes);
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.next, None);
    assert_eq!(found.text.len(), bytes.as_slice().len(), "page byte length");
    let first_diff = found
        .text
        .as_bytes()
        .iter()
        .zip(bytes.as_slice().iter())
        .position(|(a, b)| a != b);
    assert_eq!(first_diff, None, "page first differing offset");
}

#[test]
fn sixty_four_lines_of_1025_bytes_take_sixty_three() {
    let bytes = lined(LINES_USIZE, 1025);
    let (_temp, path) = staged(&bytes);
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.next, Some(63 * 1025));
    assert_eq!(
        found.text.len(),
        bytes.get(..63 * 1025).unwrap().len(),
        "page byte length"
    );
    let first_diff = found
        .text
        .as_bytes()
        .iter()
        .zip(bytes.get(..63 * 1025).unwrap().iter())
        .position(|(a, b)| a != b);
    assert_eq!(first_diff, None, "page first differing offset");
    // The rest starts the next page whole.
    let rest = page_at_path(&path, 63 * 1025).unwrap();
    assert_eq!(rest.next, None);
    assert_eq!(rest.text.as_bytes(), bytes.get(63 * 1025..).unwrap());
}

#[test]
fn a_short_line_then_a_longer_than_cap_line() {
    let mut bytes = b"a\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'b', 65537));
    bytes.push(b'\n');
    let (_temp, path) = staged(&bytes);
    let first = page_at_path(&path, 0).unwrap();
    assert_eq!(first.text, "a\n");
    assert_eq!(first.next, Some(2));
    // The long line begins the next page and is cut at the cap.
    let cut = page_at_path(&path, 2).unwrap();
    assert_eq!(cut.text.len(), BYTES_USIZE);
    assert_eq!(cut.next, Some(2 + 65536));
    let tail = page_at_path(&path, 2 + 65536).unwrap();
    assert_eq!(tail.text, "b\n");
    assert_eq!(tail.next, None);
}

#[test]
fn a_character_cut_by_the_cap_ends_the_page_at_its_lead() {
    // A line of 65535 `a` then `é`, then a tail: the first page ends at
    // the lead byte, the second starts with the character.
    let mut bytes = vec![b'a'; 65535];
    bytes.extend_from_slice("é tail\n".as_bytes());
    let (_temp, path) = staged(&bytes);
    let first = page_at_path(&path, 0).unwrap();
    assert_eq!(first.next, Some(65535));
    assert_eq!(first.text.len(), 65535);
    let second = page_at_path(&path, 65535).unwrap();
    assert!(
        second.text.starts_with('é'),
        "starts with {:?}",
        second.text.chars().take(4).collect::<String>()
    );
    // At each position of a 3-byte and a 4-byte character across the cut,
    // the first page ends at or before the cap and paging on from its
    // `next` reproduces the rest of the file.
    for tail in ["\u{20ac} tail\n", "\u{1f600} tail\n"] {
        let tail = tail.as_bytes();
        for at in 0..=3u64 {
            let mut bytes = vec![b'a'; 65536 - usize::try_from(at).unwrap()];
            bytes.extend_from_slice(tail);
            let (_temp, path) = staged(&bytes);
            let first = page_at_path(&path, 0).unwrap();
            let edge = first.next.unwrap();
            assert!(edge <= 65536, "{tail:?} at {at}");
            let edge_usize = usize::try_from(edge).unwrap();
            assert_eq!(
                first.text.len(),
                bytes.get(..edge_usize).unwrap().len(),
                "page byte length"
            );
            let first_diff = first
                .text
                .as_bytes()
                .iter()
                .zip(bytes.get(..edge_usize).unwrap().iter())
                .position(|(a, b)| a != b);
            assert_eq!(first_diff, None, "page first differing offset");
            let mut offset = edge;
            let mut rest = Vec::new();
            loop {
                let found = page_at_path(&path, offset).unwrap();
                rest.extend_from_slice(found.text.as_bytes());
                match found.next {
                    Some(at) => offset = at,
                    None => break,
                }
            }
            assert_eq!(rest.as_slice(), bytes.get(edge_usize..).unwrap());
        }
    }
}

#[test]
fn a_final_line_needs_no_newline_and_crlf_is_kept() {
    let (_temp, path) = staged(b"a\nb");
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.text, "a\nb");
    assert_eq!(found.next, None);
    let (_temp, path) = staged(b"a\r\nb\r\n");
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.text, "a\r\nb\r\n");
    assert_eq!(found.next, None);
}

#[test]
fn invalid_bytes_become_replacement_while_counts_stay_bytes() {
    let bytes = b"a\xff\nb\x80c\n";
    let (_temp, path) = staged(bytes);
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.text, "a�\nb�c\n");
    assert_eq!(found.text.len(), "a�\nb�c\n".len());
    assert_eq!(found.next, None);
    assert_eq!(found.size, u64::try_from(bytes.len()).unwrap());
}

#[test]
fn a_single_stray_byte_is_lossy_with_no_next() {
    let (_temp, path) = staged(b"\xff");
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.text, "�");
    assert_eq!(found.next, None);
    assert_eq!(found.size, 1);
}

#[test]
fn two_hundred_lines_page_to_the_end_in_four_pages() {
    let mut bytes = Vec::new();
    for i in 0..200 {
        bytes.extend_from_slice(format!("line {i:03} with some text\n").as_bytes());
    }
    let (_temp, path) = staged(&bytes);
    let mut offset = 0;
    let mut text = String::new();
    let mut pages = 0;
    let mut size;
    loop {
        let found = page_at_path(&path, offset).unwrap();
        size = found.size;
        text.push_str(&found.text);
        pages += 1;
        match found.next {
            Some(at) => {
                assert!(at > offset && at <= size, "{at} after {offset}");
                offset = at;
            }
            None => break,
        }
        assert!(pages < 10, "paged past the end");
    }
    assert_eq!(pages, 4);
    assert_eq!(text.as_bytes(), bytes.as_slice());
    assert_eq!(size, u64::try_from(bytes.len()).unwrap());
}

#[test]
fn a_file_truncated_after_the_size_capture_is_io() {
    let temp = Temp::new();
    let path = temp.write("a.txt", b"hello\nworld\n");
    let file = temp.open("a.txt");
    let size = file.metadata().unwrap().len();
    let failed = page_at(
        file,
        0,
        size,
        Some(&|| {
            File::create(&path).unwrap().write_all(b"").unwrap();
        }),
    );
    assert!(matches!(failed, Err(PageError::Io(_))));
}

#[test]
fn lines_hold_absolute_starts_past_a_nonzero_offset() {
    let mut bytes = vec![b'x'; 100];
    bytes.extend_from_slice(b"C\nDE\n");
    let (_temp, path) = staged(&bytes);
    let second = page_at_path(&path, 100).unwrap();
    assert_eq!(second.lines, vec![100, 102]);
    assert_eq!(second.text, "C\nDE\n");
}

#[test]
fn lines_pin_offsets_cap_cut_malformed_and_empty() {
    let (_temp, path) = staged(b"a\nb\n");
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.lines, vec![0, 2]);
    // 64-entry cap: 65 lines take 64.
    let (_temp, path) = staged(&lined(65, 2));
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.lines.len(), 64);
    assert_eq!(found.lines[0], 0);
    assert_eq!(found.lines[63], 63 * 2);
    // Character-boundary cut: first page ends at the lead byte.
    let mut bytes = vec![b'a'; 65535];
    bytes.extend_from_slice("é tail\n".as_bytes());
    let (_temp, path) = staged(&bytes);
    let first = page_at_path(&path, 0).unwrap();
    assert_eq!(first.lines, vec![0]);
    // Malformed bytes map to raw offsets.
    let (_temp, path) = staged(b"\xffA\nB\n");
    let found = page_at_path(&path, 0).unwrap();
    assert_eq!(found.lines, vec![0, 3]);
    assert_eq!(found.text, "�A\nB\n");
    // Empty page holds no starts.
    let (_temp, path) = staged(b"");
    let found = page_at_path(&path, 0).unwrap();
    assert!(found.lines.is_empty());
    let (_temp, path) = staged(b"hello\n");
    let found = page_at_path(&path, 6).unwrap();
    assert!(found.lines.is_empty());
}

#[test]
fn a_file_grown_after_the_size_capture_pages_to_the_capture() {
    let temp = Temp::new();
    let path = temp.write("a.txt", b"hello\n");
    let file = temp.open("a.txt");
    let size = file.metadata().unwrap().len();
    let found = page_at(
        file,
        0,
        size,
        Some(&|| {
            OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"world\n")
                .unwrap();
        }),
    )
    .unwrap();
    assert_eq!(found.text, "hello\n");
    assert_eq!(found.next, None);
    assert_eq!(found.size, size);
    // The next call sees the new size.
    let grown = page_at_path(&path, 0).unwrap();
    assert_eq!(grown.text, "hello\nworld\n");
    assert_eq!(grown.next, None);
}
