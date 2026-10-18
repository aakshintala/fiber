//! What a job delivery's hooks need: the jobs the session started,
//! folded from the log's own `job_started` and `delegate_started` lines
//! (`docs/adr/0001-session-log-is-the-only-state-of-record.md`). The loop
//! thread alone reads and writes it, on the loop's `write` path.

use std::collections::HashMap;
use std::path::PathBuf;

use contract::JobId;
use contract::events::Event;

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
            | Event::TurnStarted(_)
            | Event::StepStarted(_)
            | Event::TurnCompleted(_)
            | Event::SteeringApplied(_)
            | Event::SteeringQueue(_)
            | Event::ShellCommand(_)
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

    /// The job's start, when the log recorded one. Test-only until the
    /// delivery dispatch reads the cache.
    #[cfg(test)]
    pub(crate) fn get(&self, job: &JobId) -> Option<&Started> {
        self.jobs.get(job)
    }
}
