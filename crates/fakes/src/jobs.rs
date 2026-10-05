//! A [`contract::jobs::Jobs`] whose files live in a directory a test chooses.
//! Each completion is delivered on a channel so a test can wait for it.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use contract::JobId;
use contract::events::{JobCompleted, JobStarted};
use contract::jobs::{End, Jobs, OpenError, Opened, Opening};

struct Inner {
    next: u64,
    started: Vec<JobStarted>,
    stops: Vec<(JobId, Arc<dyn Fn() + Send + Sync>)>,
}

/// Jobs a test opens. Files are `{dir}/{job_id}.log`. [`FakeJobs::failing`]
/// refuses every open.
pub struct FakeJobs {
    dir: PathBuf,
    fail: bool,
    inner: Mutex<Inner>,
    completed_tx: Sender<JobCompleted>,
    completed_rx: Mutex<Receiver<JobCompleted>>,
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
            inner: Mutex::new(Inner {
                next: 0,
                started: Vec::new(),
                stops: Vec::new(),
            }),
            completed_tx,
            completed_rx: Mutex::new(completed_rx),
        })
    }

    /// Every job opened so far, in open order.
    pub fn started(&self) -> Vec<JobStarted> {
        lock(&self.inner).started.clone()
    }

    /// Calls the [`Stop`] the opener gave for `job_id`. An unknown id does
    /// nothing.
    pub fn stop(&self, job_id: &JobId) {
        let stop = {
            let inner = lock(&self.inner);
            inner
                .stops
                .iter()
                .find(|(id, _)| id == job_id)
                .map(|(_, stop)| Arc::clone(stop))
        };
        if let Some(stop) = stop {
            stop();
        }
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
        let end = End::new(
            job_id,
            Box::new(move |completed| {
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
        })
    }
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
