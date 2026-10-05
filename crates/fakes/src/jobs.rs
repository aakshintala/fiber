//! A [`contract::jobs::Jobs`] whose files live in a directory a test chooses.
//! Each completion is delivered on a channel so a test can wait for it.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use contract::JobId;
use contract::emit::Emit;
use contract::events::{Class, Event, JobCompleted, JobStarted};
use contract::jobs::{End, Foreground, Jobs, OpenError, Opened, Opening};

struct Inner {
    next: u64,
    started: Vec<JobStarted>,
    stops: Vec<(JobId, Arc<dyn Fn() + Send + Sync>)>,
    /// Jobs whose end was reported.
    ended: Vec<JobId>,
    /// Jobs whose stop was sent.
    stopped: Vec<JobId>,
    foreground: Vec<Weak<dyn Fn() -> bool + Send + Sync>>,
}

/// Jobs a test opens. Files are `{dir}/{job_id}.log`. [`FakeJobs::failing`]
/// refuses every open.
pub struct FakeJobs {
    dir: PathBuf,
    fail: bool,
    inner: Arc<Mutex<Inner>>,
    completed_tx: Sender<JobCompleted>,
    completed_rx: Mutex<Receiver<JobCompleted>>,
    deltas: Arc<JobDeltas>,
}

/// The `job_delta` lines every job of a [`FakeJobs`] emitted, in order.
#[derive(Default)]
pub struct JobDeltas {
    texts: Mutex<Vec<(JobId, String)>>,
    changed: Condvar,
}

impl JobDeltas {
    /// Each delta's job and text so far, in arrival order.
    pub fn deltas(&self) -> Vec<(JobId, String)> {
        lock(&self.texts).clone()
    }

    /// The concatenated delta text so far.
    pub fn text(&self) -> String {
        text_of(&lock(&self.texts))
    }

    /// Waits, at most `within` of real time, until the concatenated text
    /// contains `needle`. True once it does; false at the deadline.
    pub fn wait_for_text(&self, needle: &str, within: Duration) -> bool {
        let texts = lock(&self.texts);
        let (texts, _) = self
            .changed
            .wait_timeout_while(texts, within, |texts| !text_of(texts).contains(needle))
            .unwrap_or_else(PoisonError::into_inner);
        text_of(&texts).contains(needle)
    }
}

impl Emit for JobDeltas {
    fn emit(&self, event: &Event) {
        if event.class() != Class::Ephemeral {
            return;
        }
        if let Event::JobDelta(delta) = event {
            lock(&self.texts).push((
                delta.job_id.clone(),
                delta.progress.text.clone().unwrap_or_default(),
            ));
            self.changed.notify_all();
        }
    }
}

fn text_of(texts: &[(JobId, String)]) -> String {
    texts.iter().map(|(_, text)| text.as_str()).collect()
}

impl FakeJobs {
    /// Opens files under `dir`. `dir` must already exist.
    pub fn new(dir: &Path) -> Arc<Self> {
        Self::build(dir.to_path_buf(), false)
    }

    /// Every [`Jobs::open`] fails with [`OpenError::Io`]. Nothing is recorded.
    pub fn failing() -> Arc<Self> {
        Self::build(PathBuf::from("jobs"), true)
    }

    fn build(dir: PathBuf, fail: bool) -> Arc<Self> {
        let (completed_tx, completed_rx) = mpsc::channel();
        Arc::new(Self {
            dir,
            fail,
            inner: Arc::new(Mutex::new(Inner {
                next: 0,
                started: Vec::new(),
                stops: Vec::new(),
                ended: Vec::new(),
                stopped: Vec::new(),
                foreground: Vec::new(),
            })),
            completed_tx,
            completed_rx: Mutex::new(completed_rx),
            deltas: Arc::default(),
        })
    }

    /// Every job opened so far, in open order.
    pub fn started(&self) -> Vec<JobStarted> {
        lock(&self.inner).started.clone()
    }

    /// The `job_delta` lines the jobs emitted.
    pub fn deltas(&self) -> Arc<JobDeltas> {
        Arc::clone(&self.deltas)
    }

    /// The next completion, or `None` when none arrives within `within`.
    pub fn ended(&self, within: Duration) -> Option<JobCompleted> {
        lock(&self.completed_rx).recv_timeout(within).ok()
    }
}

impl Jobs for FakeJobs {
    fn open(&self, opening: Opening) -> Result<Opened, OpenError> {
        if self.fail {
            return Err(OpenError::Io {
                path: self.dir.join("unavailable.log"),
                source: std::io::Error::other("background jobs are unavailable"),
            });
        }
        let mut inner = lock(&self.inner);
        inner.next = inner.next.wrapping_add(1);
        let id = format!("j_{:016x}", inner.next);
        let path = self.dir.join(format!("{id}.log"));
        let file = match File::create(&path) {
            Ok(file) => file,
            Err(source) => return Err(OpenError::Io { path, source }),
        };
        let job_id = JobId(id.clone());
        let started = JobStarted {
            job_id: job_id.clone(),
            tool: Some(opening.tool),
            extension: None,
            description: opening.description,
            output_path: format!("{id}.log"),
        };
        inner.started.push(started.clone());
        inner
            .stops
            .push((job_id.clone(), Arc::from(opening.stop.0)));
        drop(inner);
        let tx = self.completed_tx.clone();
        let expected = job_id.clone();
        let book = Arc::clone(&self.inner);
        let end = End::new(
            job_id,
            Box::new(move |completed| {
                lock(&book).ended.push(expected.clone());
                // The same id the open minted, whatever the payload names.
                match tx.send(JobCompleted {
                    job_id: expected,
                    ..completed
                }) {
                    Ok(()) | Err(_) => {}
                }
            }),
        );
        Ok(Opened {
            started,
            path,
            file,
            end,
            emit: Arc::clone(&self.deltas) as Arc<dyn Emit>,
        })
    }

    /// Calls the [`contract::jobs::Stop`] the opener gave for `job_id`, once.
    /// True while the job runs; false, and no call, when the id is unknown
    /// or its end was reported.
    fn stop(&self, job_id: &JobId) -> bool {
        let stop = {
            let mut inner = lock(&self.inner);
            if inner.ended.contains(job_id) {
                return false;
            }
            let Some(stop) = inner
                .stops
                .iter()
                .find(|(id, _)| id == job_id)
                .map(|(_, stop)| Arc::clone(stop))
            else {
                return false;
            };
            if inner.stopped.contains(job_id) {
                return true;
            }
            inner.stopped.push(job_id.clone());
            stop
        };
        stop();
        true
    }

    fn background(&self) -> usize {
        let calls: Vec<_> = {
            let inner = lock(&self.inner);
            inner.foreground.iter().filter_map(Weak::upgrade).collect()
        };
        calls.iter().filter(|call| call()).count()
    }

    fn foreground(&self, call: Foreground) {
        let mut inner = lock(&self.inner);
        inner.foreground.retain(|call| call.strong_count() > 0);
        inner.foreground.push(call.0);
    }
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
