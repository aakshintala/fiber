//! The peer credential, the ancestor walk, the predicate walk and both
//! platform parsers. The `KERN_PROCARGS2` parser tests pin the exact word
//! slicing; the predicate-walk tests pin owner-first reads and same-owner
//! membership.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use super::{
    EffectiveUid, IncompleteWalk, KernelProcessRead, Pid, ProcessInfo, ProcessRead, ReadError,
};

/// A scripted process tree: parents, owners and argument vectors by pid,
/// with named unreadable pids and recorded argument-vector reads.
#[derive(Default)]
struct Script {
    parents: HashMap<Pid, Pid>,
    euids: HashMap<Pid, EffectiveUid>,
    argvs: HashMap<Pid, Vec<Vec<u8>>>,
    unreadable_parent: Vec<Pid>,
    unreadable_euid: Vec<Pid>,
    unreadable_argv: Vec<Pid>,
    unreadable_current: bool,
    argv_reads: std::cell::RefCell<Vec<Pid>>,
}

impl ProcessRead for Script {
    fn current_euid(&self) -> Result<EffectiveUid, ReadError> {
        if self.unreadable_current {
            return Err(ReadError::Unreadable {
                pid: Pid(0),
                what: "euid",
            });
        }
        Ok(EffectiveUid(1000))
    }

    fn euid(&self, pid: Pid) -> Result<EffectiveUid, ReadError> {
        if self.unreadable_euid.contains(&pid) {
            return Err(ReadError::Unreadable { pid, what: "euid" });
        }
        self.euids
            .get(&pid)
            .copied()
            .ok_or(ReadError::Unreadable { pid, what: "euid" })
    }

    fn parent(&self, pid: Pid) -> Result<Pid, ReadError> {
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

    fn argv(&self, pid: Pid) -> Result<Vec<Vec<u8>>, ReadError> {
        self.argv_reads.borrow_mut().push(pid);
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
        parents: [(Pid(100), Pid(50)), (Pid(50), Pid(1)), (Pid(1), Pid(0))]
            .into_iter()
            .collect(),
        euids: [(Pid(50), EffectiveUid(1000)), (Pid(1), EffectiveUid(0))]
            .into_iter()
            .collect(),
        ..Script::default()
    };
    let ancestors = super::ancestors(Pid(100), &read).unwrap();
    assert!(ancestors.contains(&ProcessInfo {
        pid: Pid(50),
        euid: EffectiveUid(1000),
    }));
    assert!(ancestors.contains(&ProcessInfo {
        pid: Pid(1),
        euid: EffectiveUid(0),
    }));
}

#[test]
fn ancestors_stop_at_a_zero_parent_without_reading_its_owner() {
    let read = Script {
        parents: [(Pid(100), Pid(0))].into_iter().collect(),
        unreadable_euid: vec![Pid(0)],
        ..Script::default()
    };
    assert_eq!(super::ancestors(Pid(100), &read).unwrap(), []);
}

#[test]
fn ancestors_name_an_unreadable_owner() {
    let read = Script {
        parents: [(Pid(100), Pid(50))].into_iter().collect(),
        unreadable_euid: vec![Pid(50)],
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(Pid(100), &read).unwrap_err(),
        IncompleteWalk { pid: Pid(50) }
    );
}

#[test]
fn ancestors_name_an_unreadable_parent() {
    let read = Script {
        parents: [(Pid(100), Pid(50))].into_iter().collect(),
        unreadable_parent: vec![Pid(50)],
        euids: [(Pid(50), EffectiveUid(1000))].into_iter().collect(),
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(Pid(100), &read).unwrap_err(),
        IncompleteWalk { pid: Pid(50) }
    );
}

#[test]
fn ancestors_name_a_repeated_pid() {
    let read = Script {
        parents: [(Pid(100), Pid(50)), (Pid(50), Pid(100))]
            .into_iter()
            .collect(),
        euids: [
            (Pid(100), EffectiveUid(1000)),
            (Pid(50), EffectiveUid(1000)),
        ]
        .into_iter()
        .collect(),
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(Pid(100), &read).unwrap_err(),
        IncompleteWalk { pid: Pid(100) }
    );
}

#[test]
fn ancestors_name_a_self_parent_pid() {
    let read = Script {
        parents: [(Pid(100), Pid(100))].into_iter().collect(),
        euids: [(Pid(100), EffectiveUid(1000))].into_iter().collect(),
        ..Script::default()
    };
    assert_eq!(
        super::ancestors(Pid(100), &read).unwrap_err(),
        IncompleteWalk { pid: Pid(100) }
    );
}

/// A chain of `depth` strict ancestors ending at PID 1: pids
/// `start + 1` through `start + depth`, then PID 1.
fn chain(start: u32, depth: usize) -> Script {
    let mut parents: HashMap<Pid, Pid> = HashMap::new();
    let mut euids: HashMap<Pid, EffectiveUid> = HashMap::new();
    let mut child = Pid(start);
    for step in 1..=depth {
        let parent = Pid(start + u32::try_from(step).expect("test chain fits in u32"));
        parents.insert(child, parent);
        euids.insert(parent, EffectiveUid(1000));
        child = parent;
    }
    parents.insert(child, Pid(1));
    euids.insert(Pid(1), EffectiveUid(0));
    Script {
        parents,
        euids,
        ..Script::default()
    }
}

#[test]
fn ancestors_allow_4096_ancestors() {
    let found = super::ancestors(Pid(10_000), &chain(10_000, 4096)).unwrap();
    assert_eq!(found.len(), 4097);
    assert_eq!(
        found.last(),
        Some(&ProcessInfo {
            pid: Pid(1),
            euid: EffectiveUid(0)
        })
    );
}

#[test]
fn ancestors_name_the_4097th_ancestor() {
    assert_eq!(
        super::ancestors(Pid(10_000), &chain(10_000, 4097)).unwrap_err(),
        IncompleteWalk { pid: Pid(14_097) }
    );
}

#[test]
fn peer_pid_of_a_socket_pair_is_the_caller() {
    let caller = Pid(std::process::id());
    assert_ne!(
        caller,
        Pid(1),
        "this test must distinguish a fabricated PID 1"
    );
    let (first, second) = UnixStream::pair().unwrap();
    assert_eq!(super::peer_pid(&first).unwrap(), caller);
    assert_eq!(super::peer_pid(&second).unwrap(), caller);
}

#[test]
fn the_caller_reads_its_own_kernel_identity() {
    let read = KernelProcessRead;
    assert_eq!(
        read.current_euid().unwrap(),
        EffectiveUid(rustix::process::geteuid().as_raw())
    );
    let pid = Pid(std::process::id());
    let caller_euid = EffectiveUid(rustix::process::geteuid().as_raw());
    assert!(
        caller_euid > EffectiveUid(1),
        "the kernel reader test needs a non-root uid"
    );
    assert_eq!(read.euid(pid).unwrap(), caller_euid);
    let ppid = Pid(u32::try_from(rustix::process::getppid().unwrap().as_raw_pid()).unwrap());
    assert!(
        ppid > Pid(1),
        "the kernel reader test needs a non-init parent"
    );
    assert_eq!(read.parent(pid).unwrap(), ppid);
    let argv = read.argv(pid).unwrap();
    let expected: Vec<Vec<u8>> = std::env::args_os()
        .map(|arg| arg.as_encoded_bytes().to_vec())
        .collect();
    assert_eq!(argv, expected);
    // A spawned child reads the same way: its parent is the caller and
    // its owner matches the caller's. Both are set at fork, so they need
    // no wait. The argument vector is exact only once the child execs: a
    // read mid-exec sees an empty vector, and a pre-exec read sees the
    // spawner's own, so the test polls for the spawned words within a wall
    // deadline (`docs/testing.md`, "Waits and timeouts"). The child runs
    // in its own process group, and the guard kills that group and reaps
    // the child on every exit path, so an assertion or timeout never leaks
    // it.
    struct ChildGuard {
        child: std::process::Child,
        group: u32,
    }
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            match fakes::kill_group(self.group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            match self.child.wait() {
                Ok(_) | Err(_) => {}
            }
            assert!(
                !fakes::group_alive(self.group),
                "the kernel reader test left its helper group behind"
            );
        }
    }
    let child = {
        use std::os::unix::process::CommandExt as _;
        std::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("the kernel reader test spawns a helper child")
    };
    let guard = ChildGuard {
        group: child.id(),
        child,
    };
    let child_pid = Pid(guard.group);
    assert_eq!(read.parent(child_pid).unwrap(), pid);
    assert_eq!(read.euid(child_pid).unwrap(), caller_euid);
    let wanted = [b"sleep".to_vec(), b"60".to_vec()];
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(10));
    let argv = loop {
        let argv = read.argv(child_pid).unwrap_or_default();
        if argv == wanted || deadline.left().is_zero() {
            break argv;
        }
        std::thread::yield_now();
    };
    assert_eq!(argv, wanted);
}

#[cfg(target_os = "linux")]
#[test]
fn status_reports_the_second_uid_field() {
    let bytes = b"Name:\ttest\nGid:\t1000\t1000\t1000\t1000\nUid:\t1000\t2000\t1000\t1000\n";
    assert_eq!(
        super::parse_status_euid(bytes, Pid(7)).unwrap(),
        EffectiveUid(2000)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn status_without_a_uid_line_is_unreadable() {
    let bytes = b"Name:\ttest\nGid:\t1000\t1000\t1000\t1000\n";
    assert_eq!(
        super::parse_status_euid(bytes, Pid(7)).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(7),
            what: "status"
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn stat_reads_the_parent_past_a_tricky_command_name() {
    let bytes = b"123 (my ) proc) R 567 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
    assert_eq!(super::parse_stat_parent(bytes, Pid(123)).unwrap(), Pid(567));
}

#[cfg(target_os = "linux")]
#[test]
fn stat_without_a_close_paren_is_unreadable() {
    let bytes = b"123 R 567";
    assert_eq!(
        super::parse_stat_parent(bytes, Pid(123)).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(123),
            what: "stat"
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cmdline_splits_on_nul_and_drops_one_trailing_nul() {
    assert_eq!(
        super::parse_cmdline_argv(b"fiber\0session\0--id=s_1\0", Pid(7)).unwrap(),
        [b"fiber".to_vec(), b"session".to_vec(), b"--id=s_1".to_vec()]
    );
    assert_eq!(
        super::parse_cmdline_argv(b"fiber\0session", Pid(7)).unwrap(),
        [b"fiber".to_vec(), b"session".to_vec()]
    );
    assert_eq!(
        super::parse_cmdline_argv(b"", Pid(7)).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(7),
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
        super::parse_procargs2(&buffer, Pid(7)).unwrap(),
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
            super::parse_procargs2(&buffer, Pid(7)).unwrap_err(),
            ReadError::Unreadable {
                pid: Pid(7),
                what: "argv"
            }
        );
    }
}

#[test]
fn procargs2_refuses_a_missing_or_empty_count_or_exec() {
    assert_eq!(
        super::parse_procargs2(&procargs2(0, b"/bin/fiber", 0, &[]), Pid(7)).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(7),
            what: "argv"
        }
    );
    let mut buffer = procargs2(-1, b"/bin/fiber", 0, &[]);
    assert_eq!(
        super::parse_procargs2(&buffer, Pid(7)).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(7),
            what: "argv"
        }
    );
    buffer = procargs2(1, b"", 0, &[b"session"]);
    assert_eq!(
        super::parse_procargs2(&buffer, Pid(7)).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(7),
            what: "argv"
        }
    );
}

type FakeQuery = Box<dyn FnMut(&mut [i32; 3], Option<&mut [u8]>, &mut usize) -> i32>;

fn fake_sysctl(
    data: Vec<u8>,
    probe_result: i32,
    fill_result: i32,
    probe_len: usize,
    fill_len: usize,
) -> (FakeQuery, std::rc::Rc<std::cell::Cell<usize>>) {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let called = std::rc::Rc::clone(&calls);
    let query = move |_mib: &mut [i32; 3], buffer: Option<&mut [u8]>, length: &mut usize| {
        called.set(called.get() + 1);
        match buffer {
            None => {
                *length = probe_len;
                probe_result
            }
            Some(buffer) => {
                let copied = buffer.len().min(data.len());
                buffer[..copied].copy_from_slice(&data[..copied]);
                *length = fill_len;
                fill_result
            }
        }
    };
    (Box::new(query), calls)
}

#[test]
fn sysctl_mib_orders_kernel_query_and_pid() {
    assert_eq!(super::sysctl_mib(Pid(17), 1, 49).unwrap(), [1, 49, 17]);
}

#[test]
fn sysctl_argv_reads_and_parses_the_exact_filled_bytes() {
    let data = procargs2(2, b"/bin/fiber", 3, &[b"/bin/fiber", b"session"]);
    let length = data.len();
    let (query, calls) = fake_sysctl(data, 0, 0, length, length);
    assert_eq!(
        super::argv_from_sysctl(Pid(7), [1, 49, 7], query).unwrap(),
        [b"/bin/fiber".to_vec(), b"session".to_vec()]
    );
    assert_eq!(calls.get(), 2);
}

#[test]
fn sysctl_argv_rejects_failed_zero_and_oversized_probes_without_filling() {
    for (probe_result, length) in [(1, 64), (0, 0), (0, (1 << 20) + 1)] {
        let (query, calls) = fake_sysctl(Vec::new(), probe_result, 0, length, length);
        assert_eq!(
            super::argv_from_sysctl(Pid(7), [1, 49, 7], query).unwrap_err(),
            ReadError::Unreadable {
                pid: Pid(7),
                what: "argv"
            }
        );
        assert_eq!(calls.get(), 1, "invalid probe never fills the buffer");
    }
}

#[test]
fn sysctl_argv_accepts_the_exact_size_limit() {
    let mut data = procargs2(2, b"/bin/fiber", 3, &[b"/bin/fiber", b"session"]);
    data.resize(1 << 20, 0);
    let (query, calls) = fake_sysctl(data, 0, 0, 1 << 20, 1 << 20);
    assert_eq!(
        super::argv_from_sysctl(Pid(7), [1, 49, 7], query).unwrap(),
        [b"/bin/fiber".to_vec(), b"session".to_vec()]
    );
    assert_eq!(calls.get(), 2);
}

#[test]
fn sysctl_argv_refuses_a_failed_fill() {
    let data = procargs2(2, b"/bin/fiber", 3, &[b"/bin/fiber", b"session"]);
    let length = data.len();
    let (query, calls) = fake_sysctl(data, 0, 1, length, length);
    assert_eq!(
        super::argv_from_sysctl(Pid(7), [1, 49, 7], query).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(7),
            what: "argv"
        }
    );
    assert_eq!(calls.get(), 2);
}

#[test]
fn sysctl_argv_parses_only_the_bytes_the_kernel_says_it_filled() {
    let data = procargs2(2, b"/bin/fiber", 3, &[b"/bin/fiber", b"session"]);
    let length = data.len();
    let (query, _) = fake_sysctl(data, 0, 0, length, 1);
    assert_eq!(
        super::argv_from_sysctl(Pid(7), [1, 49, 7], query).unwrap_err(),
        ReadError::Unreadable {
            pid: Pid(7),
            what: "argv"
        }
    );
}

/// A 10 -> 20 -> 30 -> 1 walk: 20 and 30 share the caller's owner and carry
/// the wanted word, while PID 1 is foreign-owned.
fn wanted_walk() -> Script {
    Script {
        parents: [
            (Pid(10), Pid(20)),
            (Pid(20), Pid(30)),
            (Pid(30), Pid(1)),
            (Pid(1), Pid(0)),
        ]
        .into_iter()
        .collect(),
        euids: [
            (Pid(20), EffectiveUid(1000)),
            (Pid(30), EffectiveUid(1000)),
            (Pid(1), EffectiveUid(0)),
        ]
        .into_iter()
        .collect(),
        argvs: [
            (Pid(20), vec![b"fiber".to_vec(), b"wanted".to_vec()]),
            (Pid(30), vec![b"fiber".to_vec(), b"wanted".to_vec()]),
        ]
        .into_iter()
        .collect(),
        ..Script::default()
    }
}

fn is_wanted(argv: &[Vec<u8>]) -> bool {
    argv.iter().any(|word| word == b"wanted")
}

#[test]
fn walk_collects_same_owner_matches() {
    let read = wanted_walk();
    let walk = super::walk_ancestors(Pid(10), &read, is_wanted).unwrap();
    assert_eq!(walk.parent_pid, Some(Pid(20)));
    // Membership without order or count: no `docs/<area>.md` line promises
    // nearest-first order or an exact count.
    assert!(walk.matched_pids.contains(&Pid(20)));
    assert!(walk.matched_pids.contains(&Pid(30)));
    // Matching each ancestor shows its same-owner arguments were read.
    assert!(read.argv_reads.borrow().contains(&Pid(20)));
    assert!(read.argv_reads.borrow().contains(&Pid(30)));
}

#[test]
fn walk_never_reads_a_foreign_owned_argv() {
    let read = Script {
        parents: [(Pid(10), Pid(20)), (Pid(20), Pid(1)), (Pid(1), Pid(0))]
            .into_iter()
            .collect(),
        euids: [(Pid(20), EffectiveUid(2000)), (Pid(1), EffectiveUid(1000))]
            .into_iter()
            .collect(),
        argvs: [(Pid(1), vec![b"fiber".to_vec(), b"wanted".to_vec()])]
            .into_iter()
            .collect(),
        ..Script::default()
    };
    // The foreign ancestor contributes nothing even without an entry: its
    // arguments are never read.
    let walk = super::walk_ancestors(Pid(10), &read, is_wanted).unwrap();
    assert_eq!(walk.parent_pid, Some(Pid(20)));
    assert!(walk.matched_pids.contains(&Pid(1)));
    let argv_reads = read.argv_reads.borrow();
    assert!(argv_reads.contains(&Pid(1)));
    assert!(!argv_reads.contains(&Pid(20)));
}

#[test]
fn walk_returns_no_matches_for_a_plain_walk() {
    let read = wanted_walk();
    let walk = super::walk_ancestors(Pid(10), &read, |_| false).unwrap();
    assert_eq!(walk.parent_pid, Some(Pid(20)));
    assert!(walk.matched_pids.is_empty());
}

#[test]
fn walk_names_an_unreadable_same_owner_argv() {
    let read = Script {
        unreadable_argv: vec![Pid(20)],
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(Pid(10), &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: Pid(20) }
    );
}

#[test]
fn walk_names_an_unreadable_owner_or_parent() {
    let read = Script {
        unreadable_euid: vec![Pid(20)],
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(Pid(10), &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: Pid(20) }
    );
    let read = Script {
        unreadable_parent: vec![Pid(30)],
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(Pid(10), &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: Pid(30) }
    );
}

#[test]
fn walk_names_the_peer_when_the_caller_owner_is_unreadable() {
    let read = Script {
        unreadable_current: true,
        ..wanted_walk()
    };
    assert_eq!(
        super::walk_ancestors(Pid(10), &read, is_wanted).unwrap_err(),
        IncompleteWalk { pid: Pid(10) }
    );
}
