//! The process's resources for a `resources` line (`docs/state.md`,
//! "Diagnostic logs"): peak and current memory plus cumulative disk I/O.
//! One sample reads every source, and any unreadable source drops the
//! whole sample: Linux reads `/proc/self/status` (`VmHWM` and `VmRSS`)
//! and `/proc/self/io` (`read_bytes` and `write_bytes`); macOS makes one
//! `proc_pid_rusage` call (`docs/performance.md`, "Measuring").

/// One all-or-nothing resource sample: peak and current memory in KiB,
/// and the storage the process has read and written since it started,
/// in bytes. `None` when any source needed for a field is unreadable,
/// so a `resources` line is never partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourcesSample {
    /// Peak memory so far, in KiB.
    pub peak_kib: u64,
    /// Current memory, in KiB.
    pub current_kib: u64,
    /// Storage read since the process started, in bytes.
    pub disk_read_bytes: u64,
    /// Storage written since the process started, in bytes.
    pub disk_write_bytes: u64,
}

/// The process's resources now, or `None` when any source is unreadable.
pub fn resources_sample() -> Option<ResourcesSample> {
    read()
}

#[cfg(target_os = "linux")]
fn read() -> Option<ResourcesSample> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let io = std::fs::read_to_string("/proc/self/io").ok()?;
    sample(&status, &io)
}

/// The combined sample for one `/proc/self/status` and one
/// `/proc/self/io` read: `None` when any of the four fields is missing
/// or malformed, so the two files are one all-or-none sample.
#[cfg(any(target_os = "linux", test))]
fn sample(status: &str, io: &str) -> Option<ResourcesSample> {
    Some(ResourcesSample {
        peak_kib: status_kib(status, "VmHWM:")?,
        current_kib: status_kib(status, "VmRSS:")?,
        disk_read_bytes: io_bytes(io, "read_bytes:")?,
        disk_write_bytes: io_bytes(io, "write_bytes:")?,
    })
}

// Mutants run on Linux, where this function is not compiled, so no test can
// reach it; the macOS CI tests run it for real.
#[cfg_attr(false, mutants::skip)]
#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "proc_pid_rusage, the only call reporting the footprint and disk I/O"
)]
fn read() -> Option<ResourcesSample> {
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
    // Status 0 means `proc_pid_rusage` wrote one `rusage_info_v4`, and
    // anything else left `info` untouched. The footprints arrive in bytes,
    // reported as whole KiB like Activity Monitor's figure; the disk
    // counters already are bytes.
    (status == 0).then_some(ResourcesSample {
        peak_kib: info.ri_lifetime_max_phys_footprint / 1024,
        current_kib: info.ri_phys_footprint / 1024,
        disk_read_bytes: info.ri_diskio_bytesread,
        disk_write_bytes: info.ri_diskio_byteswritten,
    })
}

// Mutants run on Linux, where this function is not compiled.
#[cfg_attr(false, mutants::skip)]
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read() -> Option<ResourcesSample> {
    None
}

/// The `key:` line of `/proc/self/status`, already in kB: `None` when it
/// is missing or not a whole number followed by `kB`.
#[cfg(any(target_os = "linux", test))]
fn status_kib(status: &str, key: &str) -> Option<u64> {
    let rest = status.lines().find_map(|line| line.strip_prefix(key))?;
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

/// The `key:` line of `/proc/self/io`, already in bytes: `None` when it
/// is missing or not a whole number.
#[cfg(any(target_os = "linux", test))]
fn io_bytes(io: &str, key: &str) -> Option<u64> {
    let rest = io.lines().find_map(|line| line.strip_prefix(key))?;
    let mut words = rest.split_whitespace();
    let number = words.next()?;
    if words.next().is_some() {
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
