//! Kernel process identity for socket attribution (`docs/permissions.md`,
//! "Sessions on the sockets"): the peer process id of an accepted local
//! socket, and the parent and owner walk from any process to PID 1. This
//! module knows nothing of Fiber commands; hub and doors recognize the
//! session shape at their own refusal boundaries.

use std::collections::HashSet;
use std::os::unix::net::UnixStream;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("socket process attribution is supported only on Linux and macOS");

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

/// What the walk does after the predicate sees one ancestor's argument
/// vector. The caller decides per ancestor whether to keep walking, so one
/// walk serves both a first-match probe and a collect-every-match scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visit {
    /// Not a match: keep walking toward PID 1.
    Skip,
    /// A match: record the pid and keep walking toward PID 1.
    Match,
}

/// A single kernel ancestry read, with the target's parent and every
/// matching strict ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessWalk {
    /// The first strict ancestor, or `None` when the target has no parent.
    pub parent_pid: Option<u32>,
    /// Matching strict ancestors, nearest first.
    pub matched_pids: Vec<u32>,
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

/// Every strict ancestor of `peer_pid` whose argument vector the
/// `predicate` matches, nearest first. The caller's own effective uid is
/// read first; an argument vector is read only for an ancestor sharing it,
/// so a foreign-owned ancestor contributes nothing and its arguments are
/// never read. An unreadable owner, parent, or same-owner argument vector
/// names its pid. The accepted socket peer's own arguments are never read:
/// only strict ancestors are walked.
pub fn walk_ancestors<F>(
    peer_pid: u32,
    read: &dyn ProcessRead,
    mut predicate: F,
) -> Result<ProcessWalk, IncompleteWalk>
where
    F: FnMut(&[Vec<u8>]) -> Visit,
{
    let current = read
        .current_euid()
        .map_err(|_| IncompleteWalk { pid: peer_pid })?;
    let ancestors = ancestors(peer_pid, read)?;
    let parent_pid = ancestors.first().map(|ancestor| ancestor.pid);
    let mut matched_pids = Vec::new();
    for ancestor in ancestors {
        if ancestor.euid != current {
            continue;
        }
        let argv = read
            .argv(ancestor.pid)
            .map_err(|_| IncompleteWalk { pid: ancestor.pid })?;
        match predicate(&argv) {
            Visit::Skip => {}
            Visit::Match => matched_pids.push(ancestor.pid),
        }
    }
    Ok(ProcessWalk {
        parent_pid,
        matched_pids,
    })
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
// Mutants run on Linux, where this is not compiled; the macOS test job runs it.
#[cfg_attr(false, mutants::skip)]
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

/// The parent pid and effective uid of one process from the kernel's
/// `proc_bsdinfo` (`bsd/sys/proc_info.h`): `pbi_ppid` is the parent, and
/// `pbi_uid` is the effective uid beside the real one.
#[cfg(target_os = "macos")]
// Mutants run on Linux, where this is not compiled; the macOS test job runs it.
#[cfg_attr(false, mutants::skip)]
#[allow(
    unsafe_code,
    reason = "proc_pidinfo PROC_PIDTBSDINFO into a caller-sized proc_bsdinfo: the pid converts losslessly, the flavor and zero arg are fixed, and the buffer is exactly one structure the kernel fills or fails"
)]
fn bsdinfo_of(pid: u32) -> Result<libc::proc_bsdinfo, ReadError> {
    let pid_arg =
        libc::c_int::try_from(pid).map_err(|_| ReadError::Unreadable { pid, what: "pid" })?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `pid_arg` is the wanted pid, the flavor and zero arg select
    // the bsd info of that pid, and `info` with `size` is one whole
    // `proc_bsdinfo` of plain integers the kernel fills or fails.
    let read = unsafe {
        libc::proc_pidinfo(
            pid_arg,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            size,
        )
    };
    if read != size {
        return Err(ReadError::Unreadable {
            pid,
            what: "proc_bsdinfo",
        });
    }
    // SAFETY: the call above filled every byte or failed, and the size
    // check holds a whole structure.
    Ok(unsafe { info.assume_init() })
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

#[cfg(any(target_os = "macos", test))]
fn sysctl_mib(pid: u32, ctl_kern: i32, query: i32) -> Result<[i32; 3], ReadError> {
    let pid = i32::try_from(pid).map_err(|_| ReadError::Unreadable { pid, what: "pid" })?;
    Ok([ctl_kern, query, pid])
}

#[cfg(any(target_os = "macos", test))]
fn argv_from_sysctl(
    pid: u32,
    mut mib: [i32; 3],
    mut query: impl FnMut(&mut [i32; 3], Option<&mut [u8]>, &mut usize) -> i32,
) -> Result<Vec<Vec<u8>>, ReadError> {
    let unreadable = || ReadError::Unreadable { pid, what: "argv" };
    let mut length = 0usize;
    let sized = query(&mut mib, None, &mut length);
    if sized != 0 || length == 0 || length > 1 << 20 {
        return Err(unreadable());
    }
    let mut buffer = vec![0u8; length];
    let read = query(&mut mib, Some(&mut buffer), &mut length);
    if read != 0 {
        return Err(unreadable());
    }
    buffer.truncate(length);
    parse_procargs2(&buffer, pid)
}

#[cfg(target_os = "macos")]
// Mutants run on Linux, where this is not compiled; the macOS test job runs it.
#[cfg_attr(false, mutants::skip)]
fn euid_of(pid: u32) -> Result<u32, ReadError> {
    Ok(bsdinfo_of(pid)?.pbi_uid)
}

#[cfg(target_os = "macos")]
// Mutants run on Linux, where this is not compiled; the macOS test job runs it.
#[cfg_attr(false, mutants::skip)]
fn parent_of(pid: u32) -> Result<u32, ReadError> {
    Ok(bsdinfo_of(pid)?.pbi_ppid)
}

#[cfg(target_os = "macos")]
// Mutants run on Linux, where this is not compiled; the macOS test job runs it.
#[cfg_attr(false, mutants::skip)]
#[allow(
    unsafe_code,
    reason = "sysctl KERN_PROCARGS2 with a fixed mib: the name pointer and count are fixed, the size probe passes no buffers, and the fill passes a buffer the first call sized"
)]
fn argv_of(pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
    let mib = sysctl_mib(pid, libc::CTL_KERN, libc::KERN_PROCARGS2)?;
    argv_from_sysctl(pid, mib, |mib, buffer, length| {
        let old = buffer.map_or(std::ptr::null_mut(), |bytes| {
            bytes.as_mut_ptr().cast::<libc::c_void>()
        });
        // SAFETY: `mib` is three valid ints, the size probe passes a null
        // buffer, and the fill passes the sized mutable output buffer.
        unsafe { libc::sysctl(mib.as_mut_ptr(), 3, old, length, std::ptr::null_mut(), 0) }
    })
}

#[cfg(test)]
#[path = "peer_tests.rs"]
mod tests;
