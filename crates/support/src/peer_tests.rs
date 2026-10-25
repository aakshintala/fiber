//! The peer credential, the ancestor walk and both platform parsers. The
//! macOS structure tests pin the `KERN_PROC_PID` offsets against the
//! offsets derived from `bsd/sys/sysctl.h`; the macOS agreement tests pin
//! those offsets against the running kernel.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use super::{IncompleteWalk, KernelProcessRead, ProcessInfo, ProcessRead, ReadError};

/// A scripted process tree: parents and owners by pid, with named
/// unreadable pids.
#[derive(Default)]
struct Script {
    parents: HashMap<u32, u32>,
    euids: HashMap<u32, u32>,
    unreadable_parent: Vec<u32>,
    unreadable_euid: Vec<u32>,
}

impl ProcessRead for Script {
    fn current_euid(&self) -> Result<u32, ReadError> {
        Ok(1000)
    }

    fn euid(&self, pid: u32) -> Result<u32, ReadError> {
        if self.unreadable_euid.contains(&pid) {
            return Err(ReadError::Unreadable { pid, what: "euid" });
        }
        self.euids
            .get(&pid)
            .copied()
            .ok_or(ReadError::Unreadable { pid, what: "euid" })
    }

    fn parent(&self, pid: u32) -> Result<u32, ReadError> {
        if self.unreadable_parent.contains(&pid) {
            return Err(ReadError::Unreadable {
                pid,
                what: "parent",
            });
        }
        self.parents
            .get(&pid)
            .copied()
            .ok_or(ReadError::Unreadable {
                pid,
                what: "parent",
            })
    }

    fn argv(&self, _pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
        panic!("the walk never reads an argument vector")
    }
}

#[test]
fn ancestors_walk_strict_ancestors_through_pid_1() {
    let read = Script {
        parents: [(100, 50), (50, 1), (1, 0)].into_iter().collect(),
        euids: [(50, 1000), (1, 0)].into_iter().collect(),
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(100, &read).unwrap(),
        [
            ProcessInfo {
                pid: 50,
                euid: 1000
            },
            ProcessInfo { pid: 1, euid: 0 },
        ]
    );
}

#[test]
fn ancestors_stop_at_a_zero_parent_without_reading_its_owner() {
    let read = Script {
        parents: [(100, 0)].into_iter().collect(),
        unreadable_euid: vec![0],
        ..Script::default()
    };
    assert_eq!(super::ancestors(100, &read).unwrap(), []);
}

#[test]
fn ancestors_name_an_unreadable_owner() {
    let read = Script {
        parents: [(100, 50)].into_iter().collect(),
        unreadable_euid: vec![50],
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(100, &read).unwrap_err(),
        IncompleteWalk { pid: 50 }
    );
}

#[test]
fn ancestors_name_an_unreadable_parent() {
    let read = Script {
        parents: [(100, 50)].into_iter().collect(),
        unreadable_parent: vec![50],
        euids: [(50, 1000)].into_iter().collect(),
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(100, &read).unwrap_err(),
        IncompleteWalk { pid: 50 }
    );
}

#[test]
fn ancestors_name_a_repeated_pid() {
    let read = Script {
        parents: [(100, 50), (50, 100)].into_iter().collect(),
        euids: [(100, 1000), (50, 1000)].into_iter().collect(),
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(100, &read).unwrap_err(),
        IncompleteWalk { pid: 100 }
    );
}

#[test]
fn ancestors_name_a_self_parent_pid() {
    let read = Script {
        parents: [(100, 100)].into_iter().collect(),
        euids: [(100, 1000)].into_iter().collect(),
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(100, &read).unwrap_err(),
        IncompleteWalk { pid: 100 }
    );
}

/// A chain of `depth` strict ancestors ending at PID 1: pids
/// `start + 1` through `start + depth`, then PID 1.
fn chain(start: u32, depth: usize) -> Script {
    let mut parents = HashMap::new();
    let mut euids = HashMap::new();
    let mut child = start;
    for step in 1..=depth {
        let parent = start + u32::try_from(step).expect("test chain fits in u32");
        parents.insert(child, parent);
        euids.insert(parent, 1000);
        child = parent;
    }
    parents.insert(child, 1);
    euids.insert(1, 0);
    Script {
        parents,
        euids,
        ..Script::default()
    }
}

#[test]
fn ancestors_allow_4096_ancestors() {
    let found = super::ancestors(10_000, &chain(10_000, 4096)).unwrap();
    assert_eq!(found.len(), 4097);
    assert_eq!(found.last(), Some(&ProcessInfo { pid: 1, euid: 0 }));
}

#[test]
fn ancestors_name_the_4097th_ancestor() {
    assert_eq!(
        super::ancestors(10_000, &chain(10_000, 4097)).unwrap_err(),
        IncompleteWalk { pid: 14_097 }
    );
}

#[test]
fn peer_pid_of_a_socket_pair_is_the_caller() {
    let (first, second) = UnixStream::pair().unwrap();
    assert_eq!(super::peer_pid(&first).unwrap(), std::process::id());
    assert_eq!(super::peer_pid(&second).unwrap(), std::process::id());
}

#[test]
fn the_caller_reads_its_own_kernel_identity() {
    let read = KernelProcessRead;
    assert_eq!(
        read.current_euid().unwrap(),
        rustix::process::geteuid().as_raw()
    );
    #[cfg(target_os = "linux")]
    {
        let pid = std::process::id();
        assert_eq!(read.euid(pid).unwrap(), rustix::process::geteuid().as_raw());
        let ppid = u32::try_from(rustix::process::getppid().unwrap().as_raw_pid()).unwrap();
        assert_eq!(read.parent(pid).unwrap(), ppid);
        let argv = read.argv(pid).unwrap();
        assert!(!argv.is_empty());
        assert!(!argv.first().unwrap().is_empty());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn status_reports_the_second_uid_field() {
    let bytes = b"Name:\ttest\nGid:\t1000\t1000\t1000\t1000\nUid:\t1000\t2000\t1000\t1000\n";
    assert_eq!(super::parse_status_euid(bytes, 7).unwrap(), 2000);
}

#[cfg(target_os = "linux")]
#[test]
fn status_without_a_uid_line_is_unreadable() {
    let bytes = b"Name:\ttest\nGid:\t1000\t1000\t1000\t1000\n";
    assert_eq!(
        super::parse_status_euid(bytes, 7).unwrap_err(),
        ReadError::Unreadable {
            pid: 7,
            what: "status"
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn stat_reads_the_parent_past_a_tricky_command_name() {
    let bytes = b"123 (my ) proc) R 567 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
    assert_eq!(super::parse_stat_parent(bytes, 123).unwrap(), 567);
}

#[cfg(target_os = "linux")]
#[test]
fn stat_without_a_close_paren_is_unreadable() {
    let bytes = b"123 R 567";
    assert_eq!(
        super::parse_stat_parent(bytes, 123).unwrap_err(),
        ReadError::Unreadable {
            pid: 123,
            what: "stat"
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cmdline_splits_on_nul_and_drops_one_trailing_nul() {
    assert_eq!(
        super::parse_cmdline_argv(b"fiber\0session\0--id=s_1\0", 7).unwrap(),
        [b"fiber".to_vec(), b"session".to_vec(), b"--id=s_1".to_vec()]
    );
    assert_eq!(
        super::parse_cmdline_argv(b"fiber\0session", 7).unwrap(),
        [b"fiber".to_vec(), b"session".to_vec()]
    );
    assert_eq!(
        super::parse_cmdline_argv(b"", 7).unwrap_err(),
        ReadError::Unreadable {
            pid: 7,
            what: "cmdline"
        }
    );
}

/// A `KERN_PROCARGS2` buffer: `argc`, the exec path, padding, then the
/// words and the environment that follows them.
fn procargs2(argc: i32, exec: &[u8], pad: usize, words: &[&[u8]]) -> Vec<u8> {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&argc.to_ne_bytes());
    buffer.extend_from_slice(exec);
    buffer.push(0);
    buffer.extend(std::iter::repeat_n(0, pad));
    for word in words {
        buffer.extend_from_slice(word);
        buffer.push(0);
    }
    buffer.extend_from_slice(b"ENVIRONMENT=1\0");
    buffer
}

#[test]
fn procargs2_returns_exactly_argc_words_past_the_padding() {
    let buffer = procargs2(
        3,
        b"/bin/fiber",
        5,
        &[b"/bin/fiber", b"session", b"--id=s_1"],
    );
    assert_eq!(
        super::parse_procargs2(&buffer, 7).unwrap(),
        [
            b"/bin/fiber".to_vec(),
            b"session".to_vec(),
            b"--id=s_1".to_vec()
        ]
    );
}

#[test]
fn procargs2_refuses_a_short_buffer_with_the_pid() {
    for buffer in [
        vec![],
        vec![1, 0],
        procargs2(3, b"/bin/fiber", 5, &[b"/bin/fiber"]),
    ] {
        assert_eq!(
            super::parse_procargs2(&buffer, 7).unwrap_err(),
            ReadError::Unreadable {
                pid: 7,
                what: "argv"
            }
        );
    }
}

#[test]
fn procargs2_refuses_a_missing_or_empty_count_or_exec() {
    assert_eq!(
        super::parse_procargs2(&procargs2(0, b"/bin/fiber", 0, &[]), 7).unwrap_err(),
        ReadError::Unreadable {
            pid: 7,
            what: "argv"
        }
    );
    let mut buffer = procargs2(-1, b"/bin/fiber", 0, &[]);
    assert_eq!(
        super::parse_procargs2(&buffer, 7).unwrap_err(),
        ReadError::Unreadable {
            pid: 7,
            what: "argv"
        }
    );
    buffer = procargs2(1, b"", 0, &[b"session"]);
    assert_eq!(
        super::parse_procargs2(&buffer, 7).unwrap_err(),
        ReadError::Unreadable {
            pid: 7,
            what: "argv"
        }
    );
}

/// The offsets derived from `bsd/sys/sysctl.h`: the owner word and the
/// parent word of the kernel's `kinfo_proc`.
#[test]
fn kinfo_layout_matches_the_header_derivation() {
    assert_eq!(std::mem::size_of::<super::KinfoProc>(), 568);
    assert_eq!(
        std::mem::offset_of!(super::KinfoProc, kp_eproc)
            + std::mem::offset_of!(super::Eproc, e_ucred)
            + std::mem::offset_of!(super::Ucred, cr_uid),
        420
    );
    assert_eq!(
        std::mem::offset_of!(super::KinfoProc, kp_eproc)
            + std::mem::offset_of!(super::Eproc, e_ppid),
        560
    );
}

/// A synthetic `KERN_PROC_PID` buffer with the owner and parent words set.
fn kinfo(ppid: i32, euid: u32) -> Vec<u8> {
    let mut buffer = vec![0u8; std::mem::size_of::<super::KinfoProc>()];
    let ppid_at = std::mem::offset_of!(super::KinfoProc, kp_eproc)
        + std::mem::offset_of!(super::Eproc, e_ppid);
    let euid_at = std::mem::offset_of!(super::KinfoProc, kp_eproc)
        + std::mem::offset_of!(super::Eproc, e_ucred)
        + std::mem::offset_of!(super::Ucred, cr_uid);
    buffer
        .get_mut(ppid_at..ppid_at + 4)
        .unwrap()
        .copy_from_slice(&ppid.to_ne_bytes());
    buffer
        .get_mut(euid_at..euid_at + 4)
        .unwrap()
        .copy_from_slice(&euid.to_ne_bytes());
    buffer
}

#[test]
fn kinfo_reads_the_parent_and_owner_words() {
    assert_eq!(super::parse_kinfo(&kinfo(77, 501), 7).unwrap(), (77, 501));
}

#[test]
fn kinfo_refuses_a_short_buffer_with_the_pid() {
    assert_eq!(
        super::parse_kinfo(kinfo(77, 501).get(..100).unwrap(), 7).unwrap_err(),
        ReadError::Unreadable {
            pid: 7,
            what: "kinfo"
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn kinfo_agrees_with_the_kernel_about_the_caller() {
    let pid = std::process::id();
    let info = super::kinfo_of(pid).unwrap();
    let ppid = rustix::process::getppid().unwrap().as_raw_pid();
    assert_eq!(info.kp_eproc.e_ppid, ppid);
    assert_eq!(
        info.kp_eproc.e_ucred.cr_uid,
        rustix::process::geteuid().as_raw()
    );
    let one = super::kinfo_of(1).unwrap();
    assert_eq!(one.kp_eproc.e_ppid, 0);
}
