//! The resources reader (`docs/state.md`, "Diagnostic logs"): the real
//! read on this process, and every case of the status and I/O parsers.

#![allow(clippy::unwrap_used, reason = "test code")]

use super::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_reader_reports_a_sixty_four_mebibyte_allocation() {
    let before = resources_sample().unwrap();
    // Touched on every page by the repeat constructor, so the peak
    // the reader reports must cover it; `black_box` keeps the buffer
    // alive past the reads.
    let buffer = vec![1u8; 64 << 20];
    let after = resources_sample().unwrap();
    assert!(after.peak_kib >= before.peak_kib);
    assert!(after.current_kib >= 64 * 1024, "{}", after.current_kib);
    assert!(after.current_kib > 0);
    assert!(after.peak_kib >= after.current_kib);
    std::hint::black_box(buffer);
}

#[test]
fn status_kib_reads_the_kb_figure_and_refuses_anything_else() {
    let cases: &[(&str, &str, Option<u64>)] = &[
        ("VmHWM:\t  1234 kB", "VmHWM:", Some(1234)),
        (
            "Name:\tfiber\nVmPeak:\t 9 kB\nVmHWM:\t  1234 kB\nVmRSS:\t 5 kB\n",
            "VmHWM:",
            Some(1234),
        ),
        ("VmHWM: 0 kB", "VmHWM:", Some(0)),
        ("VmHWM:\t18446744073709551615 kB", "VmHWM:", Some(u64::MAX)),
        ("VmRSS:\t  4321 kB", "VmRSS:", Some(4321)),
        ("VmHWM:\t  1234 kB\nVmRSS:\t 5 kB\n", "VmRSS:", Some(5)),
        ("", "VmHWM:", None),
        ("VmRSS:\t 5 kB\n", "VmHWM:", None),
        ("VmHWM:\t", "VmHWM:", None),
        ("VmHWM:\t 1234", "VmHWM:", None),
        ("VmHWM:\t 1234 MB", "VmHWM:", None),
        ("VmHWM:\t 1234 kB extra", "VmHWM:", None),
        ("VmHWM:\t abc kB", "VmHWM:", None),
        ("VmHWM:\t +12 kB", "VmHWM:", None),
        ("VmHWM:\t -12 kB", "VmHWM:", None),
        ("VmHWM:\t 18446744073709551616 kB", "VmHWM:", None),
        (" VmHWM:\t 12 kB", "VmHWM:", None),
    ];
    for (status, key, want) in cases {
        assert_eq!(status_kib(status, key), *want, "{status:?} {key}");
    }
}

#[test]
fn io_bytes_reads_the_byte_figure_and_refuses_anything_else() {
    let io = "rchar: 100\nwchar: 200\nread_bytes: 4096\nwrite_bytes: 8192\n";
    assert_eq!(io_bytes(io, "read_bytes:"), Some(4096));
    assert_eq!(io_bytes(io, "write_bytes:"), Some(8192));
    let cases: &[(&str, &str, Option<u64>)] = &[
        ("read_bytes: 0", "read_bytes:", Some(0)),
        (
            "read_bytes: 18446744073709551615",
            "read_bytes:",
            Some(u64::MAX),
        ),
        ("", "read_bytes:", None),
        ("read_bytes:", "read_bytes:", None),
        ("write_bytes: 1", "read_bytes:", None),
        ("read_bytes: 12 kB", "read_bytes:", None),
        ("read_bytes: 12 extra", "read_bytes:", None),
        ("read_bytes: abc", "read_bytes:", None),
        ("read_bytes: +12", "read_bytes:", None),
        ("read_bytes: -12", "read_bytes:", None),
        ("read_bytes: 18446744073709551616", "read_bytes:", None),
        (" read_bytes: 12", "read_bytes:", None),
    ];
    for (io, key, want) in cases {
        assert_eq!(io_bytes(io, key), *want, "{io:?} {key}");
    }
}

#[test]
fn the_combined_sample_needs_all_four_fields() {
    let status = "VmHWM:\t  1024 kB\nVmRSS:\t  512 kB\n";
    let io = "read_bytes: 4096\nwrite_bytes: 8192\n";
    assert_eq!(
        sample(status, io),
        Some(ResourcesSample {
            peak_kib: 1024,
            current_kib: 512,
            disk_read_bytes: 4096,
            disk_write_bytes: 8192,
        })
    );
    // Any unreadable source drops the whole sample: never a partial line.
    assert_eq!(sample("VmRSS:\t  512 kB\n", io), None);
    assert_eq!(sample(status, "write_bytes: 8192\n"), None);
    assert_eq!(sample("", ""), None);
    assert_eq!(sample(status, "read_bytes: abc\nwrite_bytes: 1\n"), None);
}
