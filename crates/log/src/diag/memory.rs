//! The process's peak memory for a `peak_memory` line (`docs/state.md`,
//! "Diagnostic logs"): peak RSS (`VmHWM`) on Linux, the peak physical
//! footprint on macOS (`docs/performance.md`, "Measuring").

/// The process's peak memory so far, in KiB, or `None` when it cannot be
/// read.
pub fn peak_kib() -> Option<u64> {
    read()
}

#[cfg(target_os = "linux")]
fn read() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| vm_hwm(&status))
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "proc_pid_rusage, the only call that reports the peak physical footprint"
)]
fn read() -> Option<u64> {
    // SAFETY: `rusage_info_v4` is plain integers, so all zeroes is a valid
    // value. `proc_pid_rusage` with `RUSAGE_INFO_V4` writes at most one
    // `rusage_info_v4` through the pointer, which points at `info`, alive
    // and exclusively borrowed for the call. `getpid` cannot fail.
    let (status, info) = unsafe {
        let mut info: libc::rusage_info_v4 = std::mem::zeroed();
        let status =
            libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&raw mut info).cast());
        (status, info)
    };
    (status == 0).then_some(info.ri_lifetime_max_phys_footprint / 1024)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read() -> Option<u64> {
    None
}

/// The `VmHWM:` line of `/proc/self/status`, already in kB: `None` when it
/// is missing or not a whole number followed by `kB`.
#[cfg(any(target_os = "linux", test))]
fn vm_hwm(status: &str) -> Option<u64> {
    let rest = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?;
    let mut words = rest.split_whitespace();
    let number = words.next()?;
    if words.next() != Some("kB") || words.next().is_some() {
        return None;
    }
    if !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
