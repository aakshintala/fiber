//! A [`contract::jobs::Jobs`] whose files live in a directory a test chooses.
//! Each completion is delivered on a channel so a test can wait for it.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use contract::JobId;
use contract::clock::Clock;
use contract::emit::Emit;
use contract::events::{Class, Event, JobCompleted, JobLine, JobStarted};
use contract::inbox::{Claim, Delivery, JobNotice};
use contract::jobs::{End, Foreground, Jobs, Lines, OpenError, Opened, Opening};
use contract::tool::Cancel;

/// A job's end not yet reported, as `jobs` keeps one: reporting consumes
/// it, and dropped unreported it reports the job failed `indeterminate`.
struct Unreported {
    job_id: JobId,
    /// Taken by the one report.
    report: Option<Box<dyn FnOnce(JobCompleted) + Send>>,
}

impl Unreported {
    fn report(mut self, completed: JobCompleted) {
        if let Some(report) = self.report.take() {
            report(completed);
        }
    }
}

impl Drop for Unreported {
    fn drop(&mut self) {
        if let Some(report) = self.report.take() {
            report(JobCompleted {
                job_id: self.job_id.clone(),
                status: contract::events::Outcome::Failed,
                error: Some(contract::shapes::Failure {
                    code: contract::ErrorCode::Indeterminate,
                    message: "The job ended without a result.".to_owned(),
                    retry_after_ms: None,
                    provider: None,
                }),
                process: None,
                output_tail: None,
            });
        }
    }
}

type Typer = Arc<dyn Fn(&[u8], &dyn Clock, &dyn Cancel) -> std::io::Result<usize> + Send + Sync>;

struct Inner {
    next: u64,
    started: Vec<JobStarted>,
    stops: Vec<(JobId, Arc<dyn Fn() + Send + Sync>)>,
    /// The terminal input of each job started with `tty`.
    inputs: Vec<(JobId, Typer)>,
    /// Jobs whose end was reported.
    ended: Vec<JobId>,
    /// Jobs whose stop was sent.
    stopped: Vec<JobId>,
    foreground: Vec<Weak<dyn Fn() -> bool + Send + Sync>>,
    /// Where each later end is sent, once [`Jobs::deliver_to`] set it.
    inbox: Option<Sender<Delivery>>,
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
    lines: Arc<JobLines>,
}

/// The batches every monitor of a [`FakeJobs`] sent, in order.
#[derive(Default)]
pub struct JobLines {
    lines: Mutex<Vec<JobLine>>,
}

impl JobLines {
    /// Every batch so far, in arrival order.
    pub fn lines(&self) -> Vec<JobLine> {
        lock(&self.lines).clone()
    }

    fn push(&self, line: JobLine) {
        lock(&self.lines).push(line);
    }
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
            .wait_timeout_while(texts, within, |texts| lacks(texts, needle))
            .unwrap_or_else(PoisonError::into_inner);
        !lacks(&texts, needle)
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

/// True while the concatenated text does not contain `needle`.
fn lacks(texts: &[(JobId, String)], needle: &str) -> bool {
    !text_of(texts).contains(needle)
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
                inputs: Vec::new(),
                ended: Vec::new(),
                stopped: Vec::new(),
                foreground: Vec::new(),
                inbox: None,
            })),
            completed_tx,
            completed_rx: Mutex::new(completed_rx),
            deltas: Arc::default(),
            lines: Arc::default(),
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

    /// The batches the monitors sent.
    pub fn lines(&self) -> Arc<JobLines> {
        Arc::clone(&self.lines)
    }

    /// Types `bytes` into `job_id`'s terminal, as `jobs write` does, with
    /// the given clock and cancel. `None` when the job is unknown, ended, or
    /// was not started with `tty`.
    pub fn type_into(
        &self,
        job_id: &JobId,
        bytes: &[u8],
        clock: &dyn Clock,
        cancel: &dyn Cancel,
    ) -> Option<std::io::Result<usize>> {
        let input = lock(&self.inner)
            .inputs
            .iter()
            .find(|(id, _)| id == job_id)
            .map(|(_, input)| Arc::clone(input))?;
        Some(input(bytes, clock, cancel))
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
        if let Some(input) = opening.input {
            inner.inputs.push((job_id.clone(), Arc::from(input.0)));
        }
        drop(inner);
        let lines = opening.lines.then(|| {
            let recorded = Arc::clone(&self.lines);
            let book = Arc::clone(&self.inner);
            Lines(Box::new(move |line: JobLine| {
                // Under the lock the end sends under, as the registry does.
                let inner = lock(&book);
                if let Some(inbox) = &inner.inbox {
                    match inbox.send(Delivery::JobLine(line.clone())) {
                        Ok(()) | Err(_) => {}
                    }
                }
                recorded.push(line);
            }))
        });
        let tx = self.completed_tx.clone();
        let expected = job_id.clone();
        let book = Arc::clone(&self.inner);
        let unreported = Unreported {
            job_id,
            report: Some(Box::new(move |completed: JobCompleted| {
                // The same id the open minted, whatever the payload names.
                let completed = JobCompleted {
                    job_id: expected.clone(),
                    ..completed
                };
                {
                    // The notice is sent under the lock that marks the job
                    // ended, so `running` never drops a job whose notice is
                    // not yet in the inbox.
                    let mut inner = lock(&book);
                    inner.ended.push(expected.clone());
                    // The terminal closes with the job.
                    inner.inputs.retain(|(id, _)| *id != expected);
                    // Nothing else claims a fake job's end, so the claim
                    // holds.
                    if let Some(inbox) = &inner.inbox {
                        match inbox.send(Delivery::Job(JobNotice {
                            completed: completed.clone(),
                            claim: Claim(Box::new(|| true)),
                            delegate: None,
                        })) {
                            Ok(()) | Err(_) => {}
                        }
                    }
                }
                match tx.send(completed) {
                    Ok(()) | Err(_) => {}
                }
            })),
        };
        let end = End(Box::new(move |completed| unreported.report(completed)));
        Ok(Opened {
            started,
            path,
            file,
            end,
            emit: Arc::clone(&self.deltas) as Arc<dyn Emit>,
            lines,
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

    /// Every job opened whose end was not reported, in open order.
    fn running(&self) -> Vec<JobId> {
        let inner = lock(&self.inner);
        inner
            .started
            .iter()
            .map(|started| started.job_id.clone())
            .filter(|id| !inner.ended.contains(id))
            .collect()
    }

    /// Each later end is also sent to `inbox` as a [`Delivery::Job`], whose
    /// claim holds, and each later batch as a [`Delivery::JobLine`].
    fn deliver_to(&self, inbox: Sender<Delivery>) {
        lock(&self.inner).inbox = Some(inbox);
    }
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
