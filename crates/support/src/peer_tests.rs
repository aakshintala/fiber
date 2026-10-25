//! The peer credential, the ancestor walk, the predicate walk and both
//! platform parsers. The `KERN_PROCARGS2` parser tests pin the exact word
//! slicing; the predicate-walk tests pin owner-first reads, nearest-first
//! matches and first-match stops.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use super::{IncompleteWalk, KernelProcessRead, ProcessInfo, ProcessRead, ReadError, Visit};

/// A scripted process tree: parents, owners and argument vectors by pid,
/// with named unreadable pids and a count of argument-vector reads.
#[derive(Default)]
struct Script {
    parents: HashMap<u32, u32>,
    euids: HashMap<u32, u32>,
    argvs: HashMap<u32, Vec<Vec<u8>>>,
    unreadable_parent: Vec<u32>,
    unreadable_euid: Vec<u32>,
    unreadable_argv: Vec<u32>,
    unreadable_current: bool,
    argv_read: std::cell::Cell<usize>,
}

impl ProcessRead for Script {
    fn current_euid(&self) -> Result<u32, ReadError> {
        if self.unreadable_current {
            return Err(ReadError::Unreadable {
                pid: 0,
                what: "euid",
            });
        }
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

    fn argv(&self, pid: u32) -> Result<Vec<Vec<u8>>, ReadError> {
        self.argv_read.set(self.argv_read.get() + 1);
        if self.unreadable_argv.contains(&pid) {
            return Err(ReadError::Unreadable { pid, what: "argv" });
        }
        self.argvs
            .get(&pid)
            .cloned()
            .ok_or(ReadError::Unreadable { pid, what: "argv" })
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

/// A 10 -> 20 -> 30 -> 1 walk: 20 and 30 share the caller's owner and carry
/// the wanted word, while PID 1 is foreign-owned.
fn wanted_walk() -> Script {
    Script {
        parents: [(10, 20), (20, 30), (30, 1), (1, 0)].into_iter().collect(),
        euids: [(20, 1000), (30, 1000), (1, 0)].into_iter().collect(),
        argvs: [
            (20, vec![b"fiber".to_vec(), b"wanted".to_vec()]),
            (30, vec![b"fiber".to_vec(), b"wanted".to_vec()]),
        ]
        .into_iter()
        .collect(),
        ..Script::default()
    }
}

fn is_wanted(argv: &[Vec<u8>]) -> Visit {
    if argv.iter().any(|word| word == b"wanted") {
        Visit::Match
    } else {
        Visit::Skip
    }
}

#[test]
fn walk_collects_same_owner_matches_nearest_first() {
    let read = wanted_walk();
    assert_eq!(
        super::walk_ancestors(10, &read, is_wanted).unwrap(),
        [20, 30]
    );
    // Both same-owner argument vectors were read, and only those two.
    assert_eq!(read.argv_read.get(), 2);
}

#[test]
fn walk_never_reads_a_foreign_owned_argv() {
    let read = Script {
        parents: [(10, 20), (20, 1), (1, 0)].into_iter().collect(),
        euids: [(20, 2000), (1, 1000)].into_iter().collect(),
        argvs: [(1, vec![b"fiber".to_vec(), b"wanted".to_vec()])]
            .into_iter()
            .collect(),
        ..Script::default()
    };
    // The foreign ancestor contributes nothing even without an entry: its
    // arguments are never read, so only PID 1's vector is read.
    assert_eq!(super::walk_ancestors(10, &read, is_wanted).unwrap(), [1]);
    assert_eq!(read.argv_read.get(), 1);
}

#[test]
fn walk_stops_on_the_first_match() {
    let read = wanted_walk();
    let stop_first = |argv: &[Vec<u8>]| {
        if is_wanted(argv) == Visit::Match {
            Visit::MatchStop
        } else {
            Visit::Skip
        }
    };
    assert_eq!(super::walk_ancestors(10, &read, stop_first).unwrap(), [20]);
    // The walk stopped at the first match: PID 30's vector was never read.
    assert_eq!(read.argv_read.get(), 1);
}

#[test]
fn walk_returns_no_matches_for_a_plain_walk() {
    let read = wanted_walk();
    assert_eq!(
        super::walk_ancestors(10, &read, |_| Visit::Skip).unwrap(),
        Vec::<u32>::new()
    );
}

#[test]
fn walk_names_an_unreadable_same_owner_argv() {
    let read = Script {
        unreadable_argv: vec![20],
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(10, &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: 20 }
    );
}

#[test]
fn walk_names_an_unreadable_owner_or_parent() {
    let read = Script {
        unreadable_euid: vec![20],
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(10, &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: 20 }
    );
    let read = Script {
        unreadable_parent: vec![30],
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(10, &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: 30 }
    );
}

#[test]
fn walk_names_the_peer_when_the_caller_owner_is_unreadable() {
    let read = Script {
        unreadable_current: true,
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(10, &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: 10 }
    );
}
