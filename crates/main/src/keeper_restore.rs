//! Jobs a keeper handed over, running again in the session's registry
//! (`docs/tools.md`, "The keeper": a resume takes the log back). The keeper
//! keeps writing each job's files until the session says it is ready; from
//! then on a job's output arrives as frames, and each job's runner in
//! [`tools`] appends it after the keeper's bytes and reports the ending
//! through the job's `End`, as any job's. This module keeps the handover
//! lifecycle around those runners: validation, the kept-or-abandoned
//! verdict for lost connections, and the drop order that closes every
//! append handle before the keeper can take the files back.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use contract::JobId;
use contract::clock::Clock;
use contract::events::{Event, EventKind, JobStarted};
use contract::jobs::{End, Input, Stop};
use support::lock;
use tools::{AdoptedRun, AdoptedSink};

use crate::keeper::{Adopted, Attached, Handover, Host};

/// What the session decided about the handover, once it has: runners that
/// find the connection closed before then wait for it. A connection ends
/// with no exit when the keeper is lost, and also when this session gave the
/// handover up and dropped it, which a runner can see first. Reporting the
/// job lost in that second case would write a completion for a job the
/// keeper still runs, so the runner leaves its report here.
struct Verdict(Mutex<Decision>);

enum Decision {
    /// Not decided; the reports of runners that lost the connection wait.
    Pending(Vec<End>),
    /// The session owns the jobs for good.
    Kept,
    /// The session gave the handover up.
    Abandoned,
}

impl Verdict {
    fn pending() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Decision::Pending(Vec::new()))))
    }

    /// A runner's connection closed with no ending: the registry records
    /// the job failed `indeterminate` if the session keeps the handover,
    /// and nothing if it gave it up.
    fn lost(&self, end: End) {
        let mut decision = lock(&self.0);
        match &mut *decision {
            Decision::Pending(ends) => ends.push(end),
            Decision::Kept => {
                drop(decision);
                drop(end);
            }
            Decision::Abandoned => {
                drop(decision);
                std::mem::forget(end);
            }
        }
    }

    /// The session decided. Reports held for the answer are made, or not.
    fn settle(&self, kept: bool) {
        let next = if kept {
            Decision::Kept
        } else {
            Decision::Abandoned
        };
        let Decision::Pending(ends) = std::mem::replace(&mut *lock(&self.0), next) else {
            return;
        };
        for end in ends {
            if kept {
                drop(end);
            } else {
                std::mem::forget(end);
            }
        }
    }
}

/// Handed-over jobs recorded in the registry, their runners not started.
/// Dropped, they close every append handle and report nothing.
pub(crate) struct Restored {
    ids: Vec<JobId>,
    runners: Vec<AdoptedRun>,
    verdict: Arc<Verdict>,
}

impl Restored {
    /// The jobs now recorded, to take out of the resume's orphan candidates.
    pub(crate) fn job_ids(&self) -> &[JobId] {
        &self.ids
    }

    /// Starts every runner. Call it once the registry's inbox is wired: an
    /// ending reported before that would not reach the loop. A runner whose
    /// connection closes with no ending returns its uncalled report, which
    /// lands in the verdict for the handover's answer.
    pub(crate) fn activate(mut self) -> Active {
        let verdict = Arc::clone(&self.verdict);
        let mut sinks = Vec::with_capacity(self.runners.len());
        let threads = std::mem::take(&mut self.runners)
            .into_iter()
            .map(|runner| {
                sinks.push(runner.sink());
                let verdict = Arc::clone(&verdict);
                thread::spawn(move || {
                    if let Some(end) = runner.run() {
                        verdict.lost(end);
                    }
                })
            })
            .collect();
        Active {
            sinks,
            threads,
            verdict,
            armed: true,
        }
    }
}

/// Running runners. Dropped before [`Active::keep`], it closes every append
/// handle at once, so the keeper can take the files back.
pub(crate) struct Active {
    sinks: Vec<AdoptedSink>,
    /// The runners' threads, for a test to learn each one finished.
    #[cfg_attr(not(test), expect(dead_code, reason = "only a test joins the runners"))]
    threads: Vec<thread::JoinHandle<()>>,
    verdict: Arc<Verdict>,
    armed: bool,
}

impl Active {
    /// The handover is complete: the runners own their files for good.
    pub(crate) fn keep(mut self) {
        self.armed = false;
        self.verdict.settle(true);
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for sink in &self.sinks {
            sink.close();
        }
        // After the handles: a runner that held its report for the answer
        // finds the handover given up and reports nothing.
        self.verdict.settle(false);
    }
}

/// A handover taken from a keeper, restored and waiting to start: the
/// runners drop before the connection, so every append handle is closed
/// before the keeper can take the files back.
pub(crate) struct Handed {
    restored: Restored,
    attached: Attached,
}

/// Retains the keeper's connection, records its jobs in `registry` and takes
/// them out of `folded`'s orphan candidates, only the ones recorded. A
/// failure leaves the connection closed and no runner started.
pub(crate) fn take(
    keeper: &Arc<Host>,
    registry: &jobs::Registry,
    dir: &log::SessionDir,
    handover: Handover,
    clock: &Arc<dyn Clock>,
    folded: &mut r#loop::Resumed,
) -> io::Result<Handed> {
    let mut attached = keeper.attach_handover(handover)?;
    let restored = restore(registry, dir, clock, attached.take_jobs())?;
    folded.keep_jobs(restored.job_ids());
    Ok(Handed { restored, attached })
}

impl Handed {
    /// Starts the runners, tells the keeper the session holds the lock and
    /// is ready, and waits for it to switch every job to frames. Call it
    /// once the registry's inbox is wired. A failure closes the append
    /// handles first and the connection second.
    pub(crate) fn finish(self) -> io::Result<()> {
        self.finish_after_start(|_| {})
    }

    /// [`Handed::finish`], with `started` run once the runners are going and
    /// before the keeper is asked to acknowledge: a test's pause point.
    fn finish_after_start(self, started: impl FnOnce(&mut Active)) -> io::Result<()> {
        let Self { restored, attached } = self;
        let mut active = restored.activate();
        started(&mut active);
        match attached.ready() {
            Ok(()) => {
                active.keep();
                attached.keep();
                Ok(())
            }
            Err(error) => {
                drop(active);
                drop(attached);
                Err(error)
            }
        }
    }
}

fn invalid(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// The jobs the log started and never ended, by id. The lock is the
/// session's, so no writer adds a line while this reads.
fn outstanding(dir: &Path) -> io::Result<HashMap<JobId, JobStarted>> {
    let mut open = HashMap::new();
    let lines = log::lines(dir).map_err(io::Error::other)?;
    for line in lines {
        let line = line.map_err(io::Error::other)?;
        if !matches!(line.kind(), EventKind::JobStarted | EventKind::JobCompleted) {
            continue;
        }
        match Event::from_envelope(&line).map_err(io::Error::other)? {
            Some(Event::JobStarted(started)) => {
                open.insert(started.job_id.clone(), started);
            }
            Some(Event::JobCompleted(ended)) => {
                open.remove(&ended.job_id);
            }
            Some(_) | None => {}
        }
    }
    Ok(open)
}

/// The stderr file beside `output`, named as the job's monitor receipt says.
fn errors_beside(output: &Path) -> PathBuf {
    let stem = output
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    output.with_file_name(format!("{stem}.stderr.log"))
}

/// Records each job the keeper handed over, in the registry, under the
/// `job_started` the log already holds. Nothing is opened or recorded until
/// every job's description matches a job the log holds open and names the
/// files that job owns, so a mismatched keeper cannot make the session write
/// anywhere else. A failure after that leaves the registry as it is for the
/// jobs recorded so far; the caller abandons the whole resume.
pub(crate) fn restore(
    registry: &jobs::Registry,
    dir: &log::SessionDir,
    clock: &Arc<dyn Clock>,
    handed: Vec<Adopted>,
) -> io::Result<Restored> {
    let open = outstanding(dir.path())?;
    let mut checked = Vec::with_capacity(handed.len());
    for adopted in handed {
        let Some(started) = open.get(&adopted.job.job_id) else {
            return Err(invalid(format!(
                "the keeper handed over {}, which the log does not hold open",
                adopted.job.job_id.0
            )));
        };
        let output = dir.path().join(&started.output_path);
        if adopted.job.output_path != output {
            return Err(invalid(format!(
                "the keeper names {} for {}, not {}",
                adopted.job.output_path.display(),
                adopted.job.job_id.0,
                output.display()
            )));
        }
        if adopted
            .job
            .errors_path
            .as_ref()
            .is_some_and(|errors| *errors != errors_beside(&output))
        {
            return Err(invalid(format!(
                "the keeper names another stderr file for {}",
                adopted.job.job_id.0
            )));
        }
        checked.push((started.clone(), adopted));
    }
    let mut restored = Restored {
        ids: Vec::new(),
        runners: Vec::new(),
        verdict: Verdict::pending(),
    };
    for (started, adopted) in checked {
        let Adopted { job, events, owner } = adopted;
        // Opened before the registry records anything, so a file that
        // cannot be held leaves the job unrecorded.
        let errors = job
            .errors_path
            .as_ref()
            .map(|path| OpenOptions::new().append(true).open(path))
            .transpose()?;
        let stop = || {
            let (owner, id) = (owner.clone(), job.id.clone());
            Stop(Box::new(move || {
                owner.stop(&id);
            }))
        };
        let input = job.tty.then(|| {
            let (owner, id) = (owner.clone(), job.id.clone());
            Input(Box::new(move |bytes, _clock, cancel| {
                if cancel.is_cancelled() {
                    return Ok(0);
                }
                owner
                    .write(&id, &String::from_utf8_lossy(bytes))
                    .map_err(|failure| io::Error::other(failure.to_string()))?;
                Ok(bytes.len())
            }))
        });
        let opened = registry
            .restore(started, stop(), input, job.lines)
            .map_err(|failure| io::Error::other(failure.to_string()))?;
        let mut opened = opened;
        let original_end = opened.end;
        opened.end = End(Box::new(move |completion| {
            (original_end.0)(completion);
        }));
        restored.ids.push(job.job_id.clone());
        // The runner owns the execution from here: the registry's opened
        // job carries its recorded identity, files and report, and the
        // monitor's own stop, which a flood sends.
        restored.runners.push(AdoptedRun::new(
            opened,
            events,
            errors,
            stop(),
            Arc::clone(clock),
        ));
    }
    Ok(restored)
}

#[cfg(test)]
#[path = "keeper_restore_tests.rs"]
mod tests;
