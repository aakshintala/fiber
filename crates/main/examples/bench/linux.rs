//! What the kernel says about a process: peak RSS and context switches
//! from `/proc/<pid>/status` and `/proc/<pid>/task/*/status`, and the
//! connected Unix sockets from `/proc/net/unix` (`docs/performance.md`,
//! "Measuring"). The parsers take text and run on every platform; only the
//! reads need Linux.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

/// One thread's context switch counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) voluntary: u64,
    pub(crate) involuntary: u64,
}

/// The number at the start of `name`'s value in a status text. A missing
/// field is an error, never 0: a zero would pass an exact budget.
fn field(status: &str, name: &str) -> Result<u64, String> {
    let line = status
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
        .ok_or_else(|| format!("the status has no {name} field"))?;
    let number = line.split_whitespace().next().unwrap_or_default();
    number
        .parse()
        .map_err(|err| format!("{name} is not a number ({number:?}): {err}"))
}

/// `VmHWM`, the kernel's peak resident set size, in KiB.
pub(crate) fn vm_hwm_kib(status: &str) -> Result<u64, String> {
    field(status, "VmHWM")
}

/// A thread's voluntary and involuntary context switch counts.
pub(crate) fn switches(status: &str) -> Result<Counts, String> {
    Ok(Counts {
        voluntary: field(status, "voluntary_ctxt_switches")?,
        involuntary: field(status, "nonvoluntary_ctxt_switches")?,
    })
}

/// The switches each thread made between `before` and `after`, one
/// `{"tid", "voluntary", "involuntary"}` entry per thread present in both.
/// A thread present in only one, or a counter that went backwards, cannot
/// be compared: it is noted in `notes` as a self-check failure.
pub(crate) fn idle_switches(
    before: &BTreeMap<u32, Counts>,
    after: &BTreeMap<u32, Counts>,
    notes: &mut Vec<String>,
) -> Vec<Value> {
    let mut deltas = Vec::new();
    for (tid, start) in before {
        let Some(end) = after.get(tid) else {
            notes.push(format!("thread {tid} exited during the idle window"));
            continue;
        };
        match (
            end.voluntary.checked_sub(start.voluntary),
            end.involuntary.checked_sub(start.involuntary),
        ) {
            (Some(voluntary), Some(involuntary)) => {
                deltas.push(json!({"tid": tid, "voluntary": voluntary, "involuntary": involuntary}))
            }
            _ => notes.push(format!("thread {tid}'s switch counters went backwards")),
        }
    }
    for tid in after.keys().filter(|tid| !before.contains_key(tid)) {
        notes.push(format!("thread {tid} started during the idle window"));
    }
    deltas
}

/// Whether `/proc/net/unix` text holds a connected socket (state `03`)
/// bound to `socket`: the server side of a connection the listener at
/// `socket` accepted carries its path.
pub(crate) fn hub_connected(net_unix: &str, socket: &Path) -> bool {
    let Some(socket) = socket.to_str() else {
        return false;
    };
    net_unix.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.get(5) == Some(&"03") && fields.get(7..).is_some_and(|path| path.join(" ") == socket)
    })
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|err| format!("reading {}: {err}", path.display()))
}

/// `pid`'s peak RSS in KiB.
pub(crate) fn peak_rss_kib(pid: u32) -> Result<u64, String> {
    vm_hwm_kib(&read(
        &Path::new("/proc").join(pid.to_string()).join("status"),
    )?)
}

/// Every thread of `pid` and its switch counters, by thread id.
pub(crate) fn threads(pid: u32) -> Result<BTreeMap<u32, Counts>, String> {
    let dir = Path::new("/proc").join(pid.to_string()).join("task");
    let entries = fs::read_dir(&dir).map_err(|err| format!("reading {}: {err}", dir.display()))?;
    let mut threads = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("reading {}: {err}", dir.display()))?;
        let name = entry.file_name();
        let Some(tid) = name.to_str().and_then(|name| name.parse().ok()) else {
            continue;
        };
        threads.insert(tid, switches(&read(&entry.path().join("status"))?)?);
    }
    Ok(threads)
}

/// The current `/proc/net/unix`.
pub(crate) fn net_unix() -> Result<String, String> {
    read(Path::new("/proc/net/unix"))
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
