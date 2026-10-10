//! A Fiber home and extension sources in a temporary directory, removed when
//! dropped. Tests never touch the real home.

#![allow(dead_code, reason = "each test file uses a different part")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use extensions::{Error, Origin, Request, plan};
use serde_json::{Value, json};

/// How far each round drives the fake clock: a day past any install or git
/// bound, so the deadline, the grace and the drain all elapse.
const FAR: Duration = Duration::from_secs(24 * 3600);

/// Bounds the fake-clock jumps while driving a stalled calibration.
const ROUNDS: u32 = 30;

/// One bounded wait for the run's answer or park signal.
const ROUND: Duration = Duration::from_millis(200);

/// Rounds of waiting for the stalled run to wait on the clock: 15 waits of
/// `ROUND` are about 3 s of wall clock, the hang guard for a run that never
/// waits.
const PARK_ROUNDS: u32 = 15;

/// Rounds of waiting for the stalled process to prove it is stuck: 8 rounds
/// of two bounded waits are about 3 s of wall clock, the hang guard for a
/// stall that never appears.
const STUCK_ROUNDS: u32 = 8;

/// Waits until the stalled run parks on the clock or answers, returning an
/// early answer at once. A test moves fake time only once the run is waiting
/// on it, past its own clock check; a run that answers without ever waiting
/// fails on its own answer, not on a jump past bounds it never computed.
fn wait_parked<T: Send>(
    clock: &fakes::clock::FakeClock,
    done: &std::sync::mpsc::Receiver<T>,
) -> Option<T> {
    for _ in 0..PARK_ROUNDS {
        if !clock.parked().is_empty() {
            return None;
        }
        match done.recv_timeout(ROUND) {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled run parks on the clock");
}

/// Waits until the process holding `stall` on its command line has outlived
/// a bounded wait, returning an early answer at once. A process seen running
/// across the wait is stuck, not starting: stopping a starter reports a
/// timeout the stall never caused, so the clock moves only after the second
/// sighting. A stall that answers instead fails on its own answer.
fn await_stuck<T: Send>(done: &std::sync::mpsc::Receiver<T>, stall: &str) -> Option<T> {
    for _ in 0..STUCK_ROUNDS {
        if !fakes::matching(stall).unwrap().is_empty() {
            // Up: still up after a bounded wait means stuck, not starting;
            // an answer meanwhile ends this at once.
            match done.recv_timeout(ROUND) {
                Ok(done) => return Some(done),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the stalled run returns")
                }
            }
            if !fakes::matching(stall).unwrap().is_empty() {
                return None;
            }
        }
        match done.recv_timeout(ROUND) {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled process runs");
}

/// Waits for the run's next park after a clock advance, or returns an early
/// answer. A later clock advance cannot collapse the park's boundary.
fn wait_parked_since<T: Send>(
    clock: &fakes::clock::FakeClock,
    mark: &fakes::clock::Mark,
    done: &std::sync::mpsc::Receiver<T>,
) -> Option<T> {
    for _ in 0..PARK_ROUNDS {
        if clock.await_any_parked_since(mark, ROUND) {
            return None;
        }
        match done.try_recv() {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled run parks on the clock after an advance");
}

/// Drives `clock` until the stalled run on `done` answers: the run first
/// parks on the clock, and the process holding `stall` on its command line
/// first proves it is stuck, so whatever instant the run computes its bounds
/// at, the next jump lands past them. Each later jump waits for a fresh park
/// acknowledgement, so the deadline, grace and drain cannot collapse into
/// one instant. An early answer ends the rounds; a run that never answers
/// fails, naming the stall.
pub(crate) fn drive<T: Send>(
    clock: &fakes::clock::FakeClock,
    done: std::sync::mpsc::Receiver<T>,
    stall: &str,
) -> T {
    // Both acknowledgements precede the first jump: stopping a starter on
    // the way up reports a timeout the stall never caused.
    if let Some(done) = wait_parked(clock, &done) {
        return done;
    }
    if let Some(done) = await_stuck(&done, stall) {
        return done;
    }
    for _ in 0..ROUNDS {
        match done.try_recv() {
            Ok(done) => return done,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
        let mark = clock.advance_marked(FAR);
        if let Some(done) = wait_parked_since(clock, &mark, &done) {
            return done;
        }
    }
    panic!("the stalled run returns");
}

pub(crate) struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let root = fakes::TempDir::new("fiber-extensions");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// An extension source directory named `dir` with this manifest and
    /// these provider files.
    pub(crate) fn source(&self, dir: &str, manifest: &Value, providers: &[Value]) -> PathBuf {
        let path = self.root.path().join("src").join(dir);
        write(&path.join("extension.json"), &manifest.to_string());
        for provider in providers {
            let name = provider["name"].as_str().unwrap();
            write(
                &path.join("providers").join(format!("{name}.json")),
                &provider.to_string(),
            );
        }
        path
    }
}

pub(crate) fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

pub(crate) fn manifest(name: &str) -> Value {
    json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 })
}

/// A provider with these model ids, each on `openai-responses`.
pub(crate) fn provider(name: &str, ids: &[&str]) -> Value {
    let models: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }))
        .collect();
    json!({ "name": name, "credential": { "env": "FIBER_TEST_UNSET_KEY" }, "models": models })
}

/// Installs the extension in `source` the way `fiber extension install <path>` does
/// and returns its name.
pub(crate) fn install(home: &Path, source: &Path, fiber: &str) -> Result<String, Error> {
    let names = plan(
        home,
        &Request::Path(source.into()),
        fiber,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )?
    .commit()?;
    Ok(names.into_iter().next().unwrap())
}

/// Copies the first-party package `name` from `providers/` to `dest`,
/// replacing each `(from, to)` string in every file's text, so a test copy
/// points at its fakes.
pub(crate) fn copy_package(dest: &Path, name: &str, replacements: &[(&str, &str)]) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name);
    copy_tree(&source, dest, replacements);
}

fn copy_tree(source: &Path, dest: &Path, replacements: &[(&str, &str)]) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target, replacements);
        } else {
            let mut text = fs::read_to_string(entry.path()).unwrap();
            for (from, to) in replacements {
                text = text.replace(from, to);
            }
            fs::write(target, text).unwrap();
        }
    }
}
