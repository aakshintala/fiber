//! Kernel process identity for socket attribution (`docs/permissions.md`,
//! "Sessions on the sockets"): the peer process id of an accepted local
//! socket, and the parent and owner walk from any process to PID 1. This
//! module knows nothing of Fiber commands; hub and doors recognize the
//! session shape at their own refusal boundaries.

use std::collections::HashSet;
use std::os::unix::net::UnixStream;

/// The kernel could not report a peer process id for the socket.
#[derive(Debug, thiserror::Error)]
pub enum PeerCredentialError {
    /// The socket has no local peer process the kernel reports.
    #[error("the socket has no local peer process: {0}")]
    Unavailable(#[from] std::io::Error),
}

/// The peer process id the kernel reports for an accepted local socket:
/// `SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS. A failure is an
/// error, never silence: the caller fails closed on it.
pub fn peer_pid(stream: &UnixStream) -> Result<u32, PeerCredentialError> {
    peer_pid_inner(stream)
}

/// One strict ancestor: its pid and its kernel-reported effective uid. The
/// walk never reads argument vectors; the caller reads them only for the
/// processes whose effective uid matches its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    /// The ancestor's pid.
    pub pid: u32,
    /// The ancestor's kernel-reported effective uid.
    pub euid: u32,
}

/// A walk that could not be completed: an effective uid or a parent pid
/// that could not be read. Names the pid whose data was unreadable, so the
/// caller names it in the `from_session` rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompleteWalk {
    /// The pid whose effective uid or parent pid could not be read.
    pub pid: u32,
}

/// What a process-data read can refuse with. Every variant names the pid,
/// so [`ancestors`] and the refusal paths name it too.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReadError {
    /// The process's data could not be read.
    #[error("process {pid}: {what}")]
    Unreadable {
        /// The process whose data could not be read.
        pid: u32,
        /// Which read failed, such as `status`, `stat` or `cmdline`.
        what: &'static str,
    },
}

/// Independent kernel reads of one process's identity: the caller's own
/// effective uid, and any process's effective uid, parent pid and full
/// argument vector including `argv[0]`. Each read stands alone, so an
/// unreadable one fails without touching the others.
pub trait ProcessRead {
    /// The calling process's kernel-reported effective uid.
    fn current_euid(&self) -> Result<u32, ReadError>;
    /// The process's kernel-reported effective uid.
    fn euid(&self, pid: u32) -> Result<u32, ReadError>;
    /// The process's kernel-reported parent pid, `0` for PID 1.
    fn parent(&self, pid: u32) -> Result<u32, ReadError>;
    /// The process's full kernel argument vector, including `argv[0]`.
    fn argv(&self, pid: u32) -> Result<Vec<Vec<u8>>, ReadError>;
}

/// The kernel's own process data: `/proc` on Linux, `sysctl` on macOS.
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelProcessRead;

impl ProcessRead for KernelProcessRead {
    fn current_euid(&self) -> Result<u32, ReadError> {
        Ok(rustix::process::geteuid().as_raw())
    }

    fn euid(&self, pid: u32) -> Result<u32, ReadError> {
        euid_of(pid)
    }

    fn parent(&self, pid: u32) -> Result<u32, ReadError> {
        parent_of(pid)
    }

    fn argv(&self, pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
        argv_of(pid)
    }
}

/// Every strict ancestor of `pid` through PID 1, nearest first. Reads each
/// ancestor's effective uid and parent pid but never an argument vector.
/// An unreadable effective uid or parent pid ends the walk with that pid,
/// as does a repeated pid (a kernel-data cycle) or a 4097th ancestor
/// before PID 1 (an unbounded walk): both fail closed naming the repeated
/// or excess pid. PID 1 itself always terminates the walk.
pub fn ancestors(pid: u32, read: &dyn ProcessRead) -> Result<Vec<ProcessInfo>, IncompleteWalk> {
    let mut ancestors = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(pid);
    let mut child = pid;
    loop {
        let parent = read
            .parent(child)
            .map_err(|_| IncompleteWalk { pid: child })?;
        if parent == 0 {
            return Ok(ancestors);
        }
        if !seen.insert(parent) {
            return Err(IncompleteWalk { pid: parent });
        }
        let euid = read
            .euid(parent)
            .map_err(|_| IncompleteWalk { pid: parent })?;
        if parent == 1 {
            ancestors.push(ProcessInfo { pid: parent, euid });
            return Ok(ancestors);
        }
        if ancestors.len() >= 4096 {
            return Err(IncompleteWalk { pid: parent });
        }
        ancestors.push(ProcessInfo { pid: parent, euid });
        child = parent;
    }
}

#[cfg(target_os = "linux")]
fn peer_pid_inner(stream: &UnixStream) -> Result<u32, PeerCredentialError> {
    use std::os::unix::io::AsFd as _;
    let credential =
        rustix::net::sockopt::socket_peercred(stream.as_fd()).map_err(std::io::Error::from)?;
    let pid = u32::try_from(credential.pid.as_raw_pid())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "a negative peer pid"))?;
    Ok(pid)
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "getsockopt LOCAL_PEERPID on a borrowed Unix socket: the fd is open, the option level and name are fixed, and the out buffer is a sized int"
)]
fn peer_pid_inner(stream: &UnixStream) -> Result<u32, PeerCredentialError> {
    use std::os::unix::io::AsRawFd as _;
    let mut peer: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: the socket fd is open for the call, the level and option
    // name are the fixed LOCAL_PEERPID pair, and `peer` with `length`
    // is a sized int the kernel fills or fails.
    let read = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut peer as *mut libc::c_int).cast::<libc::c_void>(),
            &mut length,
        )
    };
    if read != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    u32::try_from(peer)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "a negative peer pid"))
        .map_err(PeerCredentialError::Unavailable)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_pid_inner(_stream: &UnixStream) -> Result<u32, PeerCredentialError> {
    Err(PeerCredentialError::Unavailable(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no peer process id on this platform",
    )))
}

#[cfg(target_os = "linux")]
fn proc_path(pid: u32, file: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/proc/{pid}/{file}"))
}

#[cfg(target_os = "linux")]
fn read_proc(pid: u32, file: &'static str) -> Result<Vec<u8>, ReadError> {
    std::fs::read(proc_path(pid, file)).map_err(|_| ReadError::Unreadable { pid, what: file })
}

/// The second value of the `Uid:` line of `/proc/<pid>/status`: the
/// effective uid, after the real uid.
#[cfg(target_os = "linux")]
fn parse_status_euid(bytes: &[u8], pid: u32) -> Result<u32, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::Unreadable {
        pid,
        what: "status",
    })?;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("Uid:") {
            let euid = fields
                .nth(1)
                .and_then(|field| field.parse::<u32>().ok())
                .ok_or(ReadError::Unreadable {
                    pid,
                    what: "status",
                })?;
            return Ok(euid);
        }
    }
    Err(ReadError::Unreadable {
        pid,
        what: "status",
    })
}

/// The parent pid after the last `)` of `/proc/<pid>/stat`: the command
/// name itself may hold spaces and parentheses.
#[cfg(target_os = "linux")]
fn parse_stat_parent(bytes: &[u8], pid: u32) -> Result<u32, ReadError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| ReadError::Unreadable { pid, what: "stat" })?;
    let after = text
        .rsplit_once(')')
        .map(|(_, tail)| tail)
        .ok_or(ReadError::Unreadable { pid, what: "stat" })?;
    after
        .split_whitespace()
        .nth(1)
        .and_then(|field| field.parse::<u32>().ok())
        .ok_or(ReadError::Unreadable { pid, what: "stat" })
}

/// The NUL-separated words of `/proc/<pid>/cmdline`: one trailing NUL ends
/// the last word rather than starting an empty one.
#[cfg(target_os = "linux")]
fn parse_cmdline_argv(bytes: &[u8], pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
    let mut words: Vec<Vec<u8>> = bytes
        .split(|byte| *byte == 0)
        .map(|word| word.to_vec())
        .collect();
    if words.last().is_some_and(|last| last.is_empty()) {
        words.pop();
    }
    if words.is_empty() {
        return Err(ReadError::Unreadable {
            pid,
            what: "cmdline",
        });
    }
    Ok(words)
}

#[cfg(target_os = "linux")]
fn euid_of(pid: u32) -> Result<u32, ReadError> {
    parse_status_euid(&read_proc(pid, "status")?, pid)
}

#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Result<u32, ReadError> {
    parse_stat_parent(&read_proc(pid, "stat")?, pid)
}

#[cfg(target_os = "linux")]
fn argv_of(pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
    parse_cmdline_argv(&read_proc(pid, "cmdline")?, pid)
}

/// The kernel's `kinfo_proc` for one pid (`bsd/sys/sysctl.h`): only the
/// prefix through the parent pid is read, but the structures mirror the
/// header in full so the offsets stay auditable.
#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct ExternProc {
    p_un: [usize; 2],
    p_vmspace: *const core::ffi::c_void,
    p_sigacts: *const core::ffi::c_void,
    p_flag: i32,
    p_stat: i8,
    p_pid: i32,
    p_oppid: i32,
    p_dupfd: i32,
    user_stack: *const i8,
    exit_thread: *mut core::ffi::c_void,
    p_debugger: i32,
    sigwait: u32,
    p_estcpu: u32,
    p_cpticks: i32,
    p_pctcpu: u32,
    p_wchan: *const core::ffi::c_void,
    p_wmesg: *const i8,
    p_swtime: u32,
    p_slptime: u32,
    p_realtimer: Itimerval,
    p_rtime: Timeval,
    p_uticks: u64,
    p_sticks: u64,
    p_iticks: u64,
    p_traceflag: i32,
    p_tracep: *const core::ffi::c_void,
    p_siglist: i32,
    p_textvp: *const core::ffi::c_void,
    p_holdcnt: i32,
    p_sigmask: u32,
    p_sigignore: u32,
    p_sigcatch: u32,
    p_priority: u8,
    p_usrpri: u8,
    p_nice: i8,
    p_comm: [i8; 17],
    p_pgrp: *const core::ffi::c_void,
    p_addr: *const core::ffi::c_void,
    p_xstat: u16,
    p_acflag: u16,
    p_ru: *const core::ffi::c_void,
}

/// The kernel's `timeval` and `itimerval`: the scheduling prefix of
/// [`ExternProc`]. Plain integers, so the layout holds on any LP64 host.
#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
}

/// The kernel's `itimerval`: two [`Timeval`]s.
#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct Itimerval {
    it_interval: Timeval,
    it_value: Timeval,
}

/// The kernel's `_pcred` (`bsd/sys/sysctl.h`): the credential pointers are
/// never dereferenced; only the inline real and saved ids are read past.
/// At 104 bytes with tail padding, it pushes `e_ucred` to 120 and `e_vm`
/// to 200.
#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct Pcred {
    pc_lock: [u8; 72],
    pc_ucred: *const core::ffi::c_void,
    p_ruid: u32,
    p_svuid: u32,
    p_rgid: u32,
    p_svgid: u32,
    p_refcnt: i32,
}

/// The kernel's `_ucred` (`bsd/sys/sysctl.h`): `cr_uid` is the effective
/// uid. `NGROUPS` is 16 (`sys/param.h`).
#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct Ucred {
    cr_ref: i32,
    cr_uid: u32,
    cr_ngroups: i16,
    cr_groups: [u32; 16],
}

/// The kernel's `vmspace` (`bsd/sys/vm.h`): an opaque prefix the parent pid
/// follows.
#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct Vmspace {
    vm_refcnt: i32,
    vm_shm: *const i8,
    vm_rssize: i32,
    vm_swrss: i32,
    vm_tsize: i32,
    vm_dsize: i32,
    vm_ssize: i32,
    vm_taddr: *const i8,
    vm_daddr: *const i8,
    vm_maxsaddr: *const i8,
}

#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct Eproc {
    e_paddr: *const core::ffi::c_void,
    e_sess: *const core::ffi::c_void,
    e_pcred: Pcred,
    e_ucred: Ucred,
    e_vm: Vmspace,
    e_ppid: i32,
    e_pgid: i32,
}

#[cfg(any(target_os = "macos", test))]
#[allow(
    dead_code,
    reason = "mirrors the kernel structure so the attribution offsets stay auditable; only the parent and owner prefix is read"
)]
#[repr(C)]
struct KinfoProc {
    kp_proc: ExternProc,
    kp_eproc: Eproc,
}

/// The parent pid and effective uid from a `KERN_PROC_PID` buffer.
#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "reads the parent and owner words of a test-sized KERN_PROC_PID buffer through the audited layout: the length check holds a full KinfoProc of plain data"
)]
fn parse_kinfo(bytes: &[u8], pid: u32) -> Result<(u32, u32), ReadError> {
    if bytes.len() < std::mem::size_of::<KinfoProc>() {
        return Err(ReadError::Unreadable { pid, what: "kinfo" });
    }
    // SAFETY: the length check holds a full `KinfoProc`, and the structure
    // is plain integers and pointers that are read, never dereferenced.
    let info = unsafe { &*bytes.as_ptr().cast::<KinfoProc>() };
    let ppid = u32::try_from(info.kp_eproc.e_ppid)
        .map_err(|_| ReadError::Unreadable { pid, what: "kinfo" })?;
    Ok((ppid, info.kp_eproc.e_ucred.cr_uid))
}

/// The next NUL-terminated word and what follows it.
#[cfg(any(target_os = "macos", test))]
fn take_word(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = bytes.iter().position(|byte| *byte == 0)?;
    let word = bytes.get(..end)?;
    let rest = bytes.get(end..)?.strip_prefix(&[0])?;
    Some((word, rest))
}

/// The argument vector from a `KERN_PROCARGS2` buffer: `argc`, the exec
/// path, zero padding, then exactly `argc` words. A short, wordless or
/// wordless-first buffer reads as unreadable, never as a shorter vector:
/// padding zeros are skipped only before the first word, so a corrupt
/// buffer fails instead of shifting words.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(bytes: &[u8], pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
    let missing = || ReadError::Unreadable { pid, what: "argv" };
    let argc_at: [u8; 4] = bytes
        .get(..4)
        .ok_or_else(missing)?
        .try_into()
        .ok()
        .ok_or_else(missing)?;
    let want = usize::try_from(i32::from_ne_bytes(argc_at))
        .ok()
        .ok_or_else(missing)?;
    if want == 0 {
        return Err(missing());
    }
    let Some((exec, mut rest)) = take_word(bytes.get(4..).ok_or_else(missing)?) else {
        return Err(missing());
    };
    if exec.is_empty() {
        return Err(missing());
    }
    while let Some(next) = rest.strip_prefix(&[0]) {
        rest = next;
    }
    let mut argv = Vec::new();
    for _ in 0..want {
        let Some((word, next)) = take_word(rest) else {
            return Err(missing());
        };
        argv.push(word.to_vec());
        rest = next;
    }
    if argv.first().is_some_and(|first| first.is_empty()) {
        return Err(missing());
    }
    Ok(argv)
}

#[cfg(target_os = "macos")]
fn sysctl_mib(pid: u32, second: libc::c_int) -> Result<[libc::c_int; 4], ReadError> {
    let pid = libc::c_int::try_from(pid).map_err(|_| ReadError::Unreadable { pid, what: "pid" })?;
    Ok([libc::CTL_KERN, libc::KERN_PROC, second, pid])
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "sysctl KERN_PROC_PID with a fixed mib and a KinfoProc-sized buffer: the name pointer and count are fixed, and the out buffer is exactly one structure the kernel fills or fails"
)]
fn kinfo_of(pid: u32) -> Result<KinfoProc, ReadError> {
    let mut mib = sysctl_mib(pid, libc::KERN_PROC_PID)?;
    let mut info = std::mem::MaybeUninit::<KinfoProc>::zeroed();
    let mut length = std::mem::size_of::<KinfoProc>() as libc::size_t;
    // SAFETY: `mib` holds four valid ints, `info` is a full `KinfoProc`
    // of plain data the kernel writes whole, and `length` carries its size.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            info.as_mut_ptr().cast::<libc::c_void>(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 || length as usize != std::mem::size_of::<KinfoProc>() {
        return Err(ReadError::Unreadable { pid, what: "kinfo" });
    }
    // SAFETY: the call above filled every byte or failed, and the length
    // check holds a whole structure.
    Ok(unsafe { info.assume_init() })
}

#[cfg(target_os = "macos")]
fn euid_of(pid: u32) -> Result<u32, ReadError> {
    Ok(kinfo_of(pid)?.kp_eproc.e_ucred.cr_uid)
}

#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Result<u32, ReadError> {
    let ppid = kinfo_of(pid)?.kp_eproc.e_ppid;
    u32::try_from(ppid).map_err(|_| ReadError::Unreadable { pid, what: "kinfo" })
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "sysctl KERN_PROCARGS2 with a fixed mib: the name pointer and count are fixed, the size probe passes no buffers, and the fill passes a buffer the first call sized"
)]
fn argv_of(pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
    let mut mib = sysctl_mib(pid, libc::KERN_PROCARGS2)?;
    let mut length: libc::size_t = 0;
    // SAFETY: `mib` holds four valid ints; both buffer arguments are null
    // with zero lengths, so the kernel writes only the needed size.
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            std::ptr::null_mut(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || length == 0 || length > 1 << 20 {
        return Err(ReadError::Unreadable { pid, what: "argv" });
    }
    let mut buffer = vec![0u8; length as usize];
    // SAFETY: `mib` is unchanged and valid, and `buffer` is `length` bytes
    // the kernel fills or fails.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buffer.as_mut_ptr().cast::<libc::c_void>(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return Err(ReadError::Unreadable { pid, what: "argv" });
    }
    buffer.truncate(length as usize);
    parse_procargs2(&buffer, pid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn euid_of(pid: u32) -> Result<u32, ReadError> {
    Err(ReadError::Unreadable { pid, what: "euid" })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_of(pid: u32) -> Result<u32, ReadError> {
    Err(ReadError::Unreadable {
        pid,
        what: "parent",
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn argv_of(pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
    Err(ReadError::Unreadable { pid, what: "argv" })
}

#[cfg(test)]
#[path = "peer_tests.rs"]
mod tests;
