//! The peak memory reader (`docs/state.md`, "Diagnostic logs"): the real
//! read on this process, and every case of the `VmHWM` parser.

#![allow(clippy::unwrap_used, reason = "test code")]

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn the_test_process_has_a_non_zero_peak() {
    assert!(peak_kib().unwrap() > 0);
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
