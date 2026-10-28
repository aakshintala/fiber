//! What a job delivery's hooks need: the started-job cache folded from
//! the log's own `job_started` and `delegate_started` lines
//! (`docs/adr/0001-session-log-is-the-only-state-of-record.md`), and the
//! `after_tool` dispatch that runs on every delivery before it joins the
//! conversation or is logged (`docs/extensions.md`, "The hook points").
//! The loop thread alone reads and writes it, on the loop's `write` path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use contract::JobId;
use contract::TurnId;
use contract::events::{CallStatus, DelegateFinished, Event, JobCompleted, JobLine, Outcome};
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, JobDelivery};
use contract::jobs::{JobRecord, Taken};
use contract::shapes::Process;
use contract::tool::Bound;
use log::{OutputRead, Unreadable};
use serde_json::Map;

use crate::Loop;
use crate::hooks::{failed_line, withheld_line};

use super::jobs::notice_text;

/// One started job: what the delivery dispatch reads.
#[derive(Debug, Default)]
pub(crate) struct Started {
    /// The tool that started the job, `""` when none did (an extension's
    /// `host.delegate`).
    pub(crate) tool: String,
    /// The job's output file, as `job_started` recorded it.
    pub(crate) output_path: PathBuf,
    /// Whether a `delegate_started` with harness `fiber` named this job:
    /// its output file is another session's log and skips the
    /// `output_file` pass.
    pub(crate) fiber: bool,
}

/// The session's started jobs, keyed by job id. An entry per started job
/// in the session; entries are never removed.
#[derive(Debug, Default)]
pub(crate) struct Starts {
    jobs: HashMap<JobId, Started>,
}

impl Starts {
    /// Folds `job_started` (`tool`, `output_path`) and the Fiber flag from
    /// a `delegate_started` whose harness is `fiber` into the cache.
    /// Anything else leaves it.
    pub(crate) fn fold(&mut self, event: &Event) {
        match event {
            Event::JobStarted(started) => {
                let entry = self.jobs.entry(started.job_id.clone()).or_default();
                entry.tool = started.tool.clone().unwrap_or_default();
                entry.output_path = PathBuf::from(&started.output_path);
            }
            Event::DelegateStarted(started) if started.harness == "fiber" => {
                self.jobs.entry(started.job_id.clone()).or_default().fiber = true;
            }
            Event::DelegateStarted(_)
            | Event::FiberStarted(_)
            | Event::FiberExited(_)
            | Event::SessionStarted(_)
            | Event::Rewound(_)
            | Event::Moved(_)
            | Event::TurnStarted(_)
            | Event::StepStarted(_)
            | Event::TurnCompleted(_)
            | Event::SteeringApplied(_)
            | Event::SteeringQueue(_)
            | Event::ShellCommand(_)
            | Event::DirectoryAdded(_)
            | Event::SessionNamed(_)
            | Event::Clients(_)
            | Event::SessionStatus(_)
            | Event::ContextAdded(_)
            | Event::AssistantMessageStarted(_)
            | Event::AssistantMessageDelta(_)
            | Event::AssistantMessageCompleted(_)
            | Event::TextCompleted(_)
            | Event::ToolCallArgumentsDelta(_)
            | Event::ReasoningStarted(_)
            | Event::ReasoningDelta(_)
            | Event::ReasoningCompleted(_)
            | Event::ToolCallRequested(_)
            | Event::ToolCallStarted(_)
            | Event::ToolCallDelta(_)
            | Event::ToolCallCompleted(_)
            | Event::PermissionRequested(_)
            | Event::PermissionResolved(_)
            | Event::InteractionRequested(_)
            | Event::InteractionResolved(_)
            | Event::RepositoryCodeOffered(_)
            | Event::RepositoryCodeResolved(_)
            | Event::UsageRecorded(_)
            | Event::QuotaNoticed(_)
            | Event::QuotaRearmed(_)
            | Event::RetryScheduled(_)
            | Event::Notice(_)
            | Event::PreambleBuilt(_)
            | Event::ModelChanged(_)
            | Event::OpeningMessage(_)
            | Event::InstructionFile(_)
            | Event::DateChanged(_)
            | Event::SkillsChanged(_)
            | Event::HandoffStarted(_)
            | Event::HandoffCompleted(_)
            | Event::SkillsResent(_)
            | Event::ContextNudged(_)
            | Event::ReviewerKept(_)
            | Event::McpServerFailed(_)
            | Event::McpServerReady(_)
            | Event::Reloaded(_)
            | Event::ExtensionsLoaded(_)
            | Event::ExtensionStateSet(_)
            | Event::ExtensionStateUnset(_)
            | Event::ExtensionUi(_)
            | Event::ExtensionMessage(_)
            | Event::ExtensionLog(_)
            | Event::ExtensionHello(_)
            | Event::ExtensionRequest(_)
            | Event::ExtensionExec(_)
            | Event::JobDelta(_)
            | Event::JobLine(_)
            | Event::DelegateFinished(_)
            | Event::JobCompleted(_)
            | Event::JobsPendingNotified(_)
            | Event::CommandAccepted(_)
            | Event::CommandRejected(_) => {}
        }
    }

    /// The job's start, when the log recorded one: the tool that started
    /// it, its output file, and whether a Fiber delegate ran it.
    pub(crate) fn get(&self, job: &JobId) -> Option<&Started> {
        self.jobs.get(job)
    }
}

/// The delivery's `changed_by`: the chain's attribution, but only when the
/// delivery changed and the attribution is non-empty. A withheld result
/// carries none, and an empty vector is never written as `[]`.
pub(crate) fn changed_by_of(
    answer_changed_by: Vec<String>,
    outcome_changed: bool,
) -> Option<Vec<String>> {
    (outcome_changed && !answer_changed_by.is_empty()).then_some(answer_changed_by)
}

/// What one delivery dispatch returns: the chain's outcome and the
/// decided `changed_by`. `None` when no hook ran.
struct AskedDelivery {
    outcome: AfterToolOutcome,
    changed_by: Option<Vec<String>>,
}

impl Loop {
    /// The tool that started `job`, or `""` when the log recorded none: an
    /// extension's `host.delegate` starts a job with no tool.
    fn delivery_tool(&self, job: &JobId) -> String {
        self.ending
            .starts
            .get(job)
            .map(|started| started.tool.clone())
            .unwrap_or_default()
    }

    /// Runs the session's `after_tool` hooks on one delivery: the answer
    /// and the decided `changed_by`, or nothing when no hook runs (no
    /// hooks, or a shutdown). Notices a `non-blocking` failure raised are
    /// written before the delivery's event; on a `Changed` answer the
    /// hook applies the hook's carried state is written too: when
    /// `artifact_counts` the delivery applies the hook's artifact text as
    /// well as its content (the delegate finish and the output file),
    /// so an artifact-only change commits state; otherwise only content
    /// counts. No `tool.hooks` scope: a job
    /// has no tool call to scope.
    fn ask_delivery(
        &mut self,
        call: AfterToolCall<'_>,
        turn: Option<&TurnId>,
        artifact_counts: bool,
    ) -> Result<Option<AskedDelivery>, crate::Error> {
        let Some(hooks) = self.hooks.clone() else {
            return Ok(None);
        };
        if self.shutting_down() {
            return Ok(None);
        }
        let AfterToolAnswer {
            outcome,
            changed_by,
            notices,
        } = hooks.after_tool(&call);
        let state = hooks.take_change_state();
        self.drain_state(&hooks, turn)?;
        for notice in notices {
            self.write(&Event::Notice(notice), turn, None)?;
        }
        let outcome_changed = match &outcome {
            AfterToolOutcome::Changed {
                content, artifact, ..
            } => content.is_some() || (artifact_counts && artifact.is_some()),
            AfterToolOutcome::Unchanged
            | AfterToolOutcome::Withheld { .. }
            | AfterToolOutcome::Failed(_) => false,
        };
        if outcome_changed {
            self.write_carried(state, turn)?;
        }
        Ok(Some(AskedDelivery {
            outcome,
            changed_by: changed_by_of(changed_by, outcome_changed),
        }))
    }

    /// A completion notice through the hooks: `notice` holds what the
    /// model is sent, `output_tail` is then absent.
    pub(crate) fn deliver_completion(
        &mut self,
        mut completed: JobCompleted,
        turn: Option<&TurnId>,
    ) -> Result<JobCompleted, crate::Error> {
        // The text the model would have got: no hook has run on this end
        // yet, so `notice` is still unset.
        let built = notice_text(&completed);
        let tool = self.delivery_tool(&completed.job_id);
        let arguments = Map::new();
        let Some(AskedDelivery {
            outcome,
            changed_by,
        }) = self.ask_delivery(
            AfterToolCall {
                tool: &tool,
                arguments: &arguments,
                status: call_status(completed.status),
                content: &built,
                details: None,
                process: completed.process.as_ref(),
                spilled: None,
                not_regular: false,
                delivery: Some(JobDelivery::Completion),
            },
            turn,
            false,
        )?
        else {
            return Ok(completed);
        };
        match outcome {
            AfterToolOutcome::Unchanged | AfterToolOutcome::Changed { content: None, .. } => {}
            AfterToolOutcome::Changed {
                content: Some(replacement),
                ..
            } => {
                completed.notice = Some(bound_notice(
                    &self.log,
                    &completed.job_id,
                    &replacement,
                    &built,
                ));
                completed.output_tail = None;
                completed.changed_by = changed_by;
            }
            AfterToolOutcome::Withheld { extension } => {
                completed.notice = Some(withheld_line(&extension));
                completed.output_tail = None;
                completed.changed_by = None;
            }
            AfterToolOutcome::Failed(failure) => {
                completed.notice = Some(failed_line(&failure));
                completed.output_tail = None;
                completed.changed_by = None;
            }
        }
        Ok(completed)
    }

    /// A monitor batch through the hooks: only `lines` is replaced, and a
    /// withheld batch keeps its `suppressed` count.
    pub(crate) fn deliver_line(
        &mut self,
        mut line: JobLine,
        turn: Option<&TurnId>,
    ) -> Result<JobLine, crate::Error> {
        let built = line.lines.clone();
        let tool = self.delivery_tool(&line.job_id);
        let arguments = Map::new();
        let Some(AskedDelivery {
            outcome,
            changed_by,
        }) = self.ask_delivery(
            AfterToolCall {
                tool: &tool,
                arguments: &arguments,
                status: CallStatus::Completed,
                content: &built,
                details: None,
                process: None,
                spilled: None,
                not_regular: false,
                delivery: Some(JobDelivery::Monitor),
            },
            turn,
            false,
        )?
        else {
            return Ok(line);
        };
        match outcome {
            AfterToolOutcome::Unchanged | AfterToolOutcome::Changed { content: None, .. } => {}
            AfterToolOutcome::Changed {
                content: Some(replacement),
                ..
            } => {
                line.lines = bound_lines(&replacement, &built);
                line.changed_by = changed_by;
            }
            AfterToolOutcome::Withheld { extension } => {
                line.lines = withheld_line(&extension);
                line.changed_by = None;
            }
            AfterToolOutcome::Failed(failure) => {
                line.lines = failed_line(&failure);
                line.changed_by = None;
            }
        }
        Ok(line)
    }

    /// A delegate's final message through the hooks: the hook sees the
    /// full, unbounded text, and its replacement is returned unbounded.
    /// The hook's artifact text is written to `artifacts/<job_id>.txt`
    /// with the display text kept separate; an artifact-only change
    /// leaves the text unchanged. The cut belongs to `calls::job_event`
    /// after it.
    pub(crate) fn deliver_finish(
        &mut self,
        mut finished: DelegateFinished,
        status: Outcome,
        process: Option<&Process>,
        turn: Option<&TurnId>,
    ) -> Result<DelegateFinished, crate::Error> {
        let tool = self.delivery_tool(&finished.job_id);
        let arguments = Map::new();
        let Some(AskedDelivery {
            outcome,
            changed_by,
        }) = self.ask_delivery(
            AfterToolCall {
                tool: &tool,
                arguments: &arguments,
                status: call_status(status),
                content: &finished.text,
                details: None,
                process,
                spilled: None,
                not_regular: false,
                delivery: Some(JobDelivery::Delegate),
            },
            turn,
            true,
        )?
        else {
            return Ok(finished);
        };
        match outcome {
            AfterToolOutcome::Unchanged
            | AfterToolOutcome::Changed {
                content: None,
                artifact: None,
                ..
            } => {}
            AfterToolOutcome::Changed {
                content, artifact, ..
            } => {
                if let Some(replacement) = content {
                    finished.text = replacement;
                }
                if let Some(artifact) = artifact {
                    let name = format!("{}.txt", finished.job_id.0);
                    let (path, _) = self.log.write_artifact(&name, artifact.as_bytes())?;
                    finished.artifact = Some(path);
                }
                finished.changed_by = changed_by;
            }
            AfterToolOutcome::Withheld { extension } => {
                finished.text = withheld_line(&extension);
                finished.artifact = None;
                finished.changed_by = None;
            }
            AfterToolOutcome::Failed(failure) => {
                finished.text = failed_line(&failure);
                finished.artifact = None;
                finished.changed_by = None;
            }
        }
        Ok(finished)
    }

    /// A claimed end through the hooks: every finish before every
    /// completion (delegate, then completion, per end). The records keep
    /// their taken order; an ahead record's finish reads the end's
    /// `status` and `process`, the only ones in scope. Writes nothing but
    /// the hook's state lines and notices.
    pub(crate) fn deliver_end(
        &mut self,
        turn: Option<&TurnId>,
        ahead: Vec<Taken>,
        completed: JobCompleted,
        delegate: Option<DelegateFinished>,
    ) -> Result<(Vec<Taken>, JobCompleted, Option<DelegateFinished>), crate::Error> {
        let status = completed.status;
        let process = completed.process.clone();
        let mut hooked: Vec<(usize, Taken)> = Vec::with_capacity(ahead.len());
        let mut completions: Vec<(usize, JobCompleted, Option<contract::ActionId>)> = Vec::new();
        for (index, taken) in ahead.into_iter().enumerate() {
            match taken.record {
                JobRecord::DelegateFinished(finished) => hooked.push((
                    index,
                    Taken {
                        record: JobRecord::DelegateFinished(self.deliver_finish(
                            finished,
                            status,
                            process.as_ref(),
                            turn,
                        )?),
                        action: taken.action,
                    },
                )),
                JobRecord::Completed(end) => completions.push((index, end, taken.action)),
                record @ (JobRecord::Started(_) | JobRecord::DelegateStarted(_)) => hooked.push((
                    index,
                    Taken {
                        record,
                        action: taken.action,
                    },
                )),
            }
        }
        let delegate = delegate
            .map(|finished| self.deliver_finish(finished, status, process.as_ref(), turn))
            .transpose()?;
        for (index, end, action) in completions {
            hooked.push((
                index,
                Taken {
                    record: JobRecord::Completed(self.deliver_completion(end, turn)?),
                    action,
                },
            ));
        }
        let completed = self.deliver_completion(completed, turn)?;
        hooked.sort_by_key(|(index, _)| *index);
        let ahead = hooked.into_iter().map(|(_, taken)| taken).collect();
        Ok((ahead, completed, delegate))
    }
}

/// A delivery's `CallStatus` from the job's `Outcome`.
fn call_status(status: Outcome) -> CallStatus {
    match status {
        Outcome::Completed => CallStatus::Completed,
        Outcome::Failed => CallStatus::Failed,
        Outcome::Cancelled => CallStatus::Cancelled,
    }
}

/// A replaced completion notice after the hook: the cut text, which is
/// what the model is sent, with the hook's full text kept in an artifact.
/// A replacement equal to the delivery as built is kept as built, so an
/// already-cut notice keeps its marker where the cut would move it.
pub(crate) fn bound_notice(log: &log::Log, id: &JobId, replacement: &str, built: &str) -> String {
    if replacement == built {
        return replacement.to_owned();
    }
    let cap = Bound::DEFAULT.start.saturating_add(Bound::DEFAULT.end);
    if replacement.len() <= cap {
        return replacement.to_owned();
    }
    log.cut_output(replacement, Bound::DEFAULT, &format!("{}-notice.txt", id.0))
        .0
}

// debt: two copies of the monitor's line and delivery limits; move them to
// `contract` when monitor.rs is next edited beyond a literal.
const LINE_LIMIT: usize = 500;
const DELIVERY_LIMIT: usize = 3_000;

/// A replaced monitor batch after the hook: each line cut at 500
/// characters with ` [cut]`, the delivery at 3,000 characters with a
/// marker, as the monitor's own batching cuts them. A replacement equal
/// to the delivery as built is kept as built, so an already-cut batch
/// keeps its marker where the cut would move it.
pub(crate) fn bound_lines(replacement: &str, built: &str) -> String {
    if replacement == built {
        return replacement.to_owned();
    }
    let joined = replacement
        .split('\n')
        .map(|line| {
            if line.chars().count() <= LINE_LIMIT {
                line.to_owned()
            } else {
                let mut cut: String = line.chars().take(LINE_LIMIT).collect();
                cut.push_str(" [cut]");
                cut
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let total = joined.chars().count();
    if total <= DELIVERY_LIMIT {
        return joined;
    }
    let mut cut: String = joined.chars().take(DELIVERY_LIMIT).collect();
    cut.push_str(&format!(
        "\n[cut: {} more characters]",
        total.saturating_sub(DELIVERY_LIMIT)
    ));
    cut
}

/// How much of the output file one read takes: 1 MiB, plus one byte to
/// tell a file just over the bound from one exactly at it.
const OUTPUT_LIMIT: u64 = 1_048_576;

/// Reads at most [`OUTPUT_LIMIT`] + 1 bytes of the file at `path`, which
/// the caller has established sits outside the session directory: the
/// outside-session fallback for [`Loop::read_output_file`]. The open
/// follows no link and never blocks on a FIFO
/// (`OFlags::NOFOLLOW | OFlags::NONBLOCK`); the `fstat` below covers the
/// descriptor actually opened, so a path swapped for a link or a FIFO
/// before the open changes nothing about what is read.
pub(crate) fn read_external(path: &Path) -> Result<OutputRead, Unreadable> {
    use std::io::Read as _;
    // No `symlink_metadata` first: the open itself refuses a link, and
    // `NONBLOCK` keeps a FIFO from blocking it. The `fstat` covers the
    // descriptor actually opened, not the path.
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            .union(rustix::fs::OFlags::NOFOLLOW)
            .union(rustix::fs::OFlags::NONBLOCK)
            .union(rustix::fs::OFlags::CLOEXEC),
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        // A link refuses with `LOOP`; a socket with `NXIO` on Linux and
        // `OPNOTSUPP` on macOS: none is a regular file, without ever
        // following or blocking on the path. Anything else is a read
        // failure (missing, unreadable).
        if error == rustix::io::Errno::LOOP
            || error == rustix::io::Errno::NXIO
            || error == rustix::io::Errno::OPNOTSUPP
        {
            Unreadable::NotRegular
        } else {
            Unreadable::Io
        }
    })?;
    let opened = rustix::fs::fstat(&fd).map_err(|_| Unreadable::Io)?;
    if rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(Unreadable::NotRegular);
    }
    let file = std::fs::File::from(fd);
    let mut capped = file.take(OUTPUT_LIMIT + 1);
    let mut bytes = Vec::new();
    capped.read_to_end(&mut bytes).map_err(|_| Unreadable::Io)?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        let size = capped
            .get_ref()
            .metadata()
            .map_err(|_| Unreadable::Io)?
            .len()
            .max(bytes.len() as u64);
        return Ok(OutputRead::Large { size });
    }
    Ok(OutputRead::Small(bytes))
}

/// The line replacing an output file the pass cannot read, plus newline.
const UNREADABLE_LINE: &str = "Output withheld: the job's output file could not be read.\n";

impl Loop {
    /// Runs the session's `after_tool` hooks on the output file of
    /// `completed`'s job, at intake: the whole file when it fits in
    /// [`OUTPUT_LIMIT`] bytes, else its path and size. A Fiber delegate's
    /// `events.jsonl` is another session's log and is skipped, as is a
    /// job the log never started. With no hooks, or under a shutdown,
    /// nothing is read. A path that is not a regular file counts as a
    /// failure of the chain's `after_tool` hooks for that pass, under
    /// each hook's own `on_failure`: a `blocking` hook withholds the
    /// path, `non-blocking` hooks keep it with a notice each, and no
    /// registered hook leaves it alone (`docs/extensions.md`, "When a
    /// hook fails").
    pub(crate) fn redact_output(
        &mut self,
        completed: &JobCompleted,
        turn: Option<&TurnId>,
    ) -> Result<(), crate::Error> {
        self.redact_output_with(completed, turn, || {})
    }

    /// [`Loop::redact_output`] with the pause point tests use to change
    /// the file between the stat and the read.
    pub(crate) fn redact_output_with(
        &mut self,
        completed: &JobCompleted,
        turn: Option<&TurnId>,
        between: impl FnOnce(),
    ) -> Result<(), crate::Error> {
        if self.hooks.is_none() || self.shutting_down() {
            return Ok(());
        }
        let Some(started) = self.ending.starts.get(&completed.job_id) else {
            return Ok(());
        };
        if started.fiber {
            return Ok(());
        }
        let tool = started.tool.clone();
        let path = self.log.output_path(&started.output_path);
        between();
        let read = match self.read_output_file(&path) {
            Ok(read) => read,
            Err(Unreadable::Io) => {
                self.replace_output_file(&path, UNREADABLE_LINE.as_bytes())?;
                return Ok(());
            }
            Err(Unreadable::NotRegular) => {
                return self.redact_not_regular(&tool, completed, turn, &path);
            }
        };
        let (content, spilled) = match &read {
            OutputRead::Small(bytes) => (String::from_utf8_lossy(bytes).into_owned(), None),
            OutputRead::Large { size } => (
                String::new(),
                Some(contract::tool::SpillFile {
                    path: path.clone(),
                    size: *size,
                }),
            ),
        };
        let arguments = Map::new();
        let Some(AskedDelivery { outcome, .. }) = self.ask_delivery(
            AfterToolCall {
                tool: &tool,
                arguments: &arguments,
                status: call_status(completed.status),
                content: &content,
                details: None,
                process: completed.process.as_ref(),
                spilled: spilled.as_ref(),
                not_regular: false,
                delivery: Some(JobDelivery::OutputFile),
            },
            turn,
            true,
        )?
        else {
            return Ok(());
        };
        // Small file: the answer's artifact text, else its content, else
        // nothing changes. Large file: only artifact text replaces it; a
        // returned content is ignored.
        let replacement: Option<Vec<u8>> = match outcome {
            AfterToolOutcome::Unchanged => None,
            AfterToolOutcome::Changed {
                content, artifact, ..
            } => {
                if let Some(artifact) = artifact {
                    Some(artifact.into_bytes())
                } else if let Some(content) = content
                    && matches!(read, OutputRead::Small(_))
                {
                    Some(content.into_bytes())
                } else {
                    None
                }
            }
            AfterToolOutcome::Withheld { extension } => {
                Some(format!("{}\n", withheld_line(&extension)).into_bytes())
            }
            AfterToolOutcome::Failed(failure) => {
                Some(format!("{}\n", failed_line(&failure)).into_bytes())
            }
        };
        if let Some(replacement) = replacement {
            self.replace_output_file(&path, &replacement)?;
        }
        Ok(())
    }

    /// Reads the job's output file at `path`: through `log` when it sits
    /// inside the session directory, by the outside-session fallback
    /// otherwise. A path inside the session directory but outside
    /// `artifacts/` fails as [`Unreadable::Io`], never reaching the
    /// fallback, so the pass cannot hand over another session file.
    fn read_output_file(&self, path: &Path) -> Result<OutputRead, Unreadable> {
        match path.strip_prefix(self.log.dir()) {
            Ok(relative) => self.log.read_output(relative, OUTPUT_LIMIT),
            Err(_) => read_external(path),
        }
    }

    /// Replaces the job's output file at `path` with `bytes`: through
    /// `log` when it sits inside the session directory, by the
    /// outside-session fallback otherwise. A path inside the session
    /// directory but outside `artifacts/` fails as [`log::Error::Io`].
    fn replace_output_file(&self, path: &Path, bytes: &[u8]) -> Result<(), crate::Error> {
        if let Ok(relative) = path.strip_prefix(self.log.dir()) {
            self.log.replace_output(relative, bytes)?;
            return Ok(());
        }
        replace_external(path, bytes)
    }

    /// Applies the synthesized failure of an `output_file` pass over a
    /// path that is not a regular file: the pass asks the chain with no
    /// input handed over, and a `blocking` answer replaces the path
    /// itself with the withheld line while any other answer keeps it.
    /// With no hooks, or under a shutdown, the path is left as it is.
    fn redact_not_regular(
        &mut self,
        tool: &str,
        completed: &JobCompleted,
        turn: Option<&TurnId>,
        path: &Path,
    ) -> Result<(), crate::Error> {
        let arguments = Map::new();
        let Some(AskedDelivery { outcome, .. }) = self.ask_delivery(
            AfterToolCall {
                tool,
                arguments: &arguments,
                status: call_status(completed.status),
                content: "",
                details: None,
                process: completed.process.as_ref(),
                spilled: None,
                delivery: Some(JobDelivery::OutputFile),
                not_regular: true,
            },
            turn,
            false,
        )?
        else {
            return Ok(());
        };
        // No input was handed over, so a change applies to nothing and
        // the path is kept: only a withheld answer replaces it.
        let replacement: Option<Vec<u8>> = match outcome {
            AfterToolOutcome::Unchanged | AfterToolOutcome::Changed { .. } => None,
            AfterToolOutcome::Withheld { extension } => {
                Some(format!("{}\n", withheld_line(&extension)).into_bytes())
            }
            AfterToolOutcome::Failed(failure) => {
                Some(format!("{}\n", failed_line(&failure)).into_bytes())
            }
        };
        if let Some(replacement) = replacement {
            self.replace_output_file(path, &replacement)?;
        }
        Ok(())
    }
}

/// Replaces the file at `path`, which the caller has established sits
/// outside the session directory, with `bytes`: the outside-session
/// fallback for [`Loop::replace_output_file`]. It keeps the original's
/// permission bits: the bytes go to a sibling file first, which is given
/// those bits and `rename`d over the path (over a link, the link). The
/// sibling is created exclusively and written through its own handle, so
/// a pre-existing sibling link never redirects the write; only a sibling
/// this call created is removed on an error path.
fn replace_external(path: &Path, bytes: &[u8]) -> Result<(), crate::Error> {
    use std::io::Write as _;
    let mut name = path
        .file_name()
        .map(|name| name.to_owned())
        .unwrap_or_default();
    name.push(".fiber-redact");
    let sibling = path.with_file_name(name);
    let permissions = std::fs::metadata(path).map(|stat| stat.permissions()).ok();
    let replaced = (|| -> std::io::Result<()> {
        // A sibling this call did not create (a job could leave it as
        // a link): unlink the name itself without following it, then
        // create exclusively. Removing an absent name fails silently,
        // so the open below reports the real error.
        let _removed = std::fs::remove_file(&sibling);
        let mut sibling_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sibling)?;
        sibling_file.write_all(bytes)?;
        sibling_file.sync_all()?;
        drop(sibling_file);
        if let Some(permissions) = permissions {
            std::fs::set_permissions(&sibling, permissions)?;
        }
        std::fs::rename(&sibling, path)?;
        Ok(())
    })();
    if replaced.is_err() {
        let _removed = std::fs::remove_file(&sibling);
    }
    replaced.map_err(|source| log::Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}
