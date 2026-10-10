//! What the kernel says about a process: peak RSS and context switches
//! from `/proc/<pid>/status` and `/proc/<pid>/task/*/status`, and the
//! connected Unix sockets from `/proc/net/unix` (`docs/performance.md`,
//! "Measuring"). The parsers take text and run on every platform; only the
//! reads need Linux.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

/// One thread's context switch counters, its name when it could be read,
/// and its state: `S` while it sleeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) voluntary: u64,
    pub(crate) involuntary: u64,
    pub(crate) name: Option<String>,
    pub(crate) state: char,
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

/// The first character of the `State:` value, `S` in `S (sleeping)`. A
/// missing or empty value is an error, never a default.
fn state(status: &str) -> Result<char, String> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))
        .and_then(|value| value.trim_start().chars().next())
        .ok_or_else(|| "the status has no State value".to_owned())
}

/// A thread's voluntary and involuntary context switch counts and state.
pub(crate) fn switches(status: &str) -> Result<Counts, String> {
    Ok(Counts {
        voluntary: field(status, "voluntary_ctxt_switches")?,
        involuntary: field(status, "nonvoluntary_ctxt_switches")?,
        name: None,
        state: state(status)?,
    })
}

/// Every thread that has not settled between two readings, in tid order:
/// one awake in `second`, one whose counters moved, and one present in only
/// one reading. Empty when every thread sleeps and none switched between
/// them.
pub(crate) fn unsettled(
    first: &BTreeMap<u32, Counts>,
    second: &BTreeMap<u32, Counts>,
) -> Vec<String> {
    let tids: std::collections::BTreeSet<&u32> = first.keys().chain(second.keys()).collect();
    tids.into_iter()
        .filter_map(|tid| match (first.get(tid), second.get(tid)) {
            (Some(_), None) => Some(format!("{tid} exited")),
            (None, Some(_)) => Some(format!("{tid} started")),
            (Some(start), Some(end)) => {
                let who = match end.name.as_ref().or(start.name.as_ref()) {
                    Some(name) => format!("{tid} ({name})"),
                    None => tid.to_string(),
                };
                if end.state != 'S' {
                    Some(format!("{who} is {}", end.state))
                } else if (start.voluntary, start.involuntary) != (end.voluntary, end.involuntary) {
                    Some(format!("{who} switched"))
                } else {
                    None
                }
            }
            (None, None) => None,
        })
        .collect()
}

/// A thread's name from its `comm` text: the one line, without its newline.
pub(crate) fn comm(text: &str) -> Result<String, String> {
    let name = text.strip_suffix('\n').unwrap_or(text);
    if name.is_empty() || name.contains('\n') {
        return Err(format!("comm is not one name: {text:?}"));
    }
    Ok(name.to_owned())
}

/// The switches each thread made between `before` and `after`, one
/// `{"tid", "voluntary", "involuntary"}` entry per thread present in both,
/// with `"name"` when the thread's name was read.
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
                let mut delta = serde_json::Map::new();
                delta.insert("tid".to_owned(), json!(tid));
                delta.insert("voluntary".to_owned(), json!(voluntary));
                delta.insert("involuntary".to_owned(), json!(involuntary));
                if let Some(name) = end.name.as_ref().or(start.name.as_ref()) {
                    delta.insert("name".to_owned(), json!(name));
                }
                deltas.push(Value::Object(delta));
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

/// Every thread of `pid` and its switch counters and name, by thread id. A
/// name that cannot be read is left out: the thread may have just exited.
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
        let mut counts = switches(&read(&entry.path().join("status"))?)?;
        counts.name = read(&entry.path().join("comm"))
            .and_then(|text| comm(&text))
            .ok();
        threads.insert(tid, counts);
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
