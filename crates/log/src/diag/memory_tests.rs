//! The peak memory reader (`docs/state.md`, "Diagnostic logs"): the real
//! read on this process, and every case of the `VmHWM` parser.

#![allow(clippy::unwrap_used, reason = "test code")]

use super::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_reader_reports_a_sixty_four_mebibyte_allocation() {
    let before = peak_kib().unwrap();
    // Touched on every page by the repeat constructor, so the peak
    // the reader reports must cover it; `black_box` keeps the buffer
    // alive past the reads.
    let buffer = vec![1u8; 64 << 20];
    let after = peak_kib().unwrap();
    assert!(after >= before, "{after} >= {before}");
    assert!(after >= 64 * 1024, "{after}");
    std::hint::black_box(buffer);
}

#[test]
fn vm_hwm_reads_the_kb_figure_and_refuses_anything_else() {
    let cases: &[(&str, Option<u64>)] = &[
        ("VmHWM:\t  1234 kB", Some(1234)),
        (
            "Name:\tfiber\nVmPeak:\t 9 kB\nVmHWM:\t  1234 kB\nVmRSS:\t 5 kB\n",
            Some(1234),
        ),
        ("VmHWM: 0 kB", Some(0)),
        ("VmHWM:\t18446744073709551615 kB", Some(u64::MAX)),
        ("", None),
        ("VmRSS:\t 5 kB\n", None),
        ("VmHWM:\t", None),
        ("VmHWM:\t 1234", None),
        ("VmHWM:\t 1234 MB", None),
        ("VmHWM:\t 1234 kB extra", None),
        ("VmHWM:\t abc kB", None),
        ("VmHWM:\t +12 kB", None),
        ("VmHWM:\t -12 kB", None),
        ("VmHWM:\t 18446744073709551616 kB", None),
        (" VmHWM:\t 12 kB", None),
    ];
    for (status, want) in cases {
        assert_eq!(vm_hwm(status), *want, "{status:?}");
    }
}
