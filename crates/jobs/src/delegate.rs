//! A Fiber delegate: spawning the child session, watching it to its end,
//! and folding how it ended (`docs/delegates.md`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contract::diag::Op;
use contract::emit::Emit;
use contract::events::{DelegateStarted, JobStarted};
use contract::jobs::JobRecord;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Point};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, JobId, SessionId};
use serde_json::{Map, Value, json};
pub(crate) mod fork;
pub(crate) mod group;
pub(crate) mod models;
pub(crate) mod outcome;
pub(crate) mod run;

pub use models::{DelegateModels, Resolve};
pub(crate) mod message;

pub use message::{DelegateMessage, LastSeq, Steer, Steered};
pub use run::{Fresh, Launch, Launched, Start, Watch, Watched};

use run::Runner;

use crate::error::JobsError;
use crate::registry::{Registry, mint_job_id};

/// Starts a Fiber delegate: a child `fiber session` that runs the prompt
/// as a job and exits when its run ends (`docs/delegates.md`, "The
/// tools"). The call returns as soon as the child has spawned and the job
/// is recorded; the runner thread owns everything after that.
///
/// The fields are wired by `main`: delegates of `parent` share
/// `workspace`, and each child's log resolves through `events_for`;
/// `bound` is the stop bound and `cap` the output cap.
// debt: the wiring fields repeat on `DelegateFork` and both `starter()`
// builders forward them field by field; move them into one shared `Wiring`
// struct embedded by each tool; the next edit that adds, removes, or
// renames any wiring field on either tool.
pub struct DelegateSpawn {
    /// This session's depth in the delegate tree.
    pub depth: u32,
    /// The session's jobs.
    pub registry: Arc<Registry>,
    /// The parent session: the child's `--parent`.
    pub parent: SessionId,
    /// The parent's workspace, which the delegate shares.
    pub workspace: PathBuf,
    /// Resolves the child's `events.jsonl` from its session id.
    pub events_for: EventsFor,
    /// The session clock.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// How long a stop waits before SIGKILL.
    pub bound: Duration,
    /// The output cap, in bytes of the delegate's `events.jsonl`.
    pub cap: u64,
    /// Resolves a `fiber:` reference, or lists the valid ones.
    pub resolve: Resolve,
    /// Builds the child's command.
    pub launch: Launch,
    /// Watches the child's socket.
    pub watch: Watch,
}

/// Starts a Fiber fork: a child `fiber session` that continues a point in
/// another session's log and exits when its run ends
/// (`docs/delegates.md`, "Forks"). Like [`DelegateSpawn`], the call
/// returns as soon as the child has spawned and the job is recorded; the
/// runner thread owns everything after that. The point the job continues
/// from arrives through `point`, read after the argument check and before
/// admission; the caller wires the reader.
///
/// The fields are wired by `main`: like a spawn's, delegates of `parent`
/// share `workspace`, and each child's log resolves through `events_for`;
/// `bound` is the stop bound and `cap` the output cap.
pub struct DelegateFork {
    /// This session's depth in the delegate tree.
    pub depth: u32,
    /// The session's jobs.
    pub registry: Arc<Registry>,
    /// The parent session: the child's `--parent`.
    pub parent: SessionId,
    /// The parent's workspace, which the fork shares.
    pub workspace: PathBuf,
    /// Resolves the child's `events.jsonl` from its session id.
    pub events_for: EventsFor,
    /// The session clock.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// How long a stop waits before SIGKILL.
    pub bound: Duration,
    /// The output cap, in bytes of the fork's `events.jsonl`.
    pub cap: u64,
    /// Reads the point the fork continues from, with the calling action
    /// when the loop runs the call (`Some`) and without it (`None`) on
    /// any other path.
    pub point: ForkPoint,
    /// Builds the child's command.
    pub launch: Launch,
    /// Watches the child's socket.
    pub watch: Watch,
}

/// Reads the point a fork continues from: the caller wires the reader,
/// and the fork only calls it. The
/// argument is the calling action, for the root walk, or `None` where no
/// call runs.
pub type ForkPoint = Arc<dyn Fn(Option<&ActionId>) -> Result<Forking, Failure> + Send + Sync>;

/// Resolves a child's `events.jsonl` from its session id: computed by
/// `main` from the sessions directory, called by the shared start with
/// the minted id. The resolver keeps the child's log beside its
/// siblings without the job knowing the sessions layout.
pub type EventsFor = Arc<dyn Fn(&SessionId) -> PathBuf + Send + Sync>;

/// The resolved point a fork continues from, plus the model reference
/// the start writes verbatim into `DelegateStarted.model`: a fork takes
/// no model argument, so nothing resolves here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forking {
    /// The point in its session's log.
    pub point: Point,
    /// The model reference, written verbatim.
    pub model: String,
}

pub(crate) const DEPTH_CAP: u32 = 2;

pub(crate) fn depth_refusal(depth: u32, tool: &str) -> Option<JobsError> {
    if depth >= DEPTH_CAP {
        Some(JobsError::DepthExceeded {
            tool: tool.to_owned(),
        })
    } else {
        None
    }
}

/// Reads `description` and `prompt`: each is required, and each is a
/// string. `model` is checked in `run`, not here, so a missing or
/// wrongly typed one reaches the tool's own check, which lists the valid
/// roles and references (`docs/delegates.md`, "Choosing a model").
#[allow(
    clippy::result_large_err,
    reason = "every jobs boundary returns the crate's error enum by value"
)]
fn check_effects_arguments(
    arguments: &Map<String, Value>,
) -> Result<(String, String, Option<Duration>), JobsError> {
    let (description, prompt) = check_pair(arguments, "description", "prompt")?;
    let timeout = match arguments.get("timeout_ms") {
        None => None,
        Some(value) => {
            let milliseconds = value
                .as_u64()
                .filter(|milliseconds| *milliseconds >= 1)
                .ok_or(JobsError::TimeoutNotPositive)?;
            Some(Duration::from_millis(milliseconds))
        }
    };
    Ok((description, prompt, timeout))
}

/// Reads each of the two names from the call's arguments: each is
/// required, and each is a string.
#[allow(
    clippy::result_large_err,
    reason = "every jobs boundary returns the crate's error enum by value"
)]
pub(crate) fn check_pair(
    arguments: &Map<String, Value>,
    first: &str,
    second: &str,
) -> Result<(String, String), JobsError> {
    let mut missing = Vec::new();
    for name in [first, second] {
        if !arguments.contains_key(name) {
            missing.push(name.to_owned());
        }
    }
    if !missing.is_empty() {
        return Err(JobsError::ArgumentsMissing { names: missing });
    }
    let mut wrong = Vec::new();
    for name in [first, second] {
        if !arguments[name].is_string() {
            wrong.push(name.to_owned());
        }
    }
    if !wrong.is_empty() {
        return Err(JobsError::ArgumentsNotStrings { names: wrong });
    }
    Ok((string(arguments, first), string(arguments, second)))
}

/// The argument's text. The caller checked it is a string; anything else
/// reads as empty and never reaches here.
fn string(arguments: &Map<String, Value>, name: &str) -> String {
    arguments[name].as_str().unwrap_or_default().to_owned()
}

pub(crate) type ExtensionStart<'a> = (&'a str, &'a mut dyn FnMut(&JobId));

pub(crate) struct Started {
    job_id: JobId,
    output_path: String,
    started: JobStarted,
    delegate: DelegateStarted,
}

/// The start's receipt: the child's job id and log path, with the start
/// records the runner's end joins later. Both delegate tools answer
/// with it.
pub(crate) fn receipt(start: Started) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: format!(
                "Started delegate {}. Its log is {}.",
                start.job_id.0, start.output_path
            ),
        }],
        jobs: vec![
            JobRecord::Started(start.started),
            JobRecord::DelegateStarted(start.delegate),
        ],
        ..Output::default()
    }
}

impl Tool for DelegateSpawn {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_spawn".to_owned(),
            description: "Starts a Fiber delegate: a child session that runs \
                 `prompt` as a background job and exits when its run ends. \
                 Returns at once with the delegate's job id and log path; \
                 wait for it with `jobs wait`. `model` is required: a configured \
                 role name or a full `fiber:` model reference, checked when the \
                 call is made; list the valid roles and references with \
                 `delegate_models`. Optional `timeout_ms` stops the delegate at its deadline, failed with code `timeout`; there is no default."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "What the delegate does, shown in `jobs list`."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "What the child session runs."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Stops the delegate after this many milliseconds; it then ends failed with code timeout. Optional, with no default."
                    },
                    "model": {
                        "description": "The delegate's model: a configured role name or a full `fiber:` reference, checked when the call is made. Required."
                    }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        check_effects_arguments(arguments)
            .map_err(|error| EffectsError::Arguments(error.to_string()))?;
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
            },
            // No subject: a rule matches this tool by name
            // (`docs/tools.md`, "What a tool declares").
            subject: Some(String::new()),
            prefix: None,
            // Always judged by the parent's reviewer: no fast path,
            // session grant or standing allow skips it
            // (`docs/delegates.md`, "Permissions").
            always_reviewed: true,
            hosts: None,
        })
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        match self.start(arguments, None) {
            Ok(start) => receipt(start),
            Err(error) => error.output(false),
        }
    }
}

impl DelegateSpawn {
    /// Starts an extension-owned delegate with the model tool's admission
    /// and limits. Binds its id before spawning; the caller must unbind on
    /// a later failure. `bind` must not start or resume another delegate.
    #[allow(
        clippy::result_large_err,
        reason = "every jobs boundary returns the crate's error enum by value"
    )]
    pub fn start_extension(
        &self,
        extension: &str,
        arguments: &Map<String, Value>,
        bind: &mut dyn FnMut(&JobId),
    ) -> Result<JobId, JobsError> {
        self.start(arguments, Some((extension, bind)))
            .map(|start| start.job_id)
    }

    #[allow(
        clippy::result_large_err,
        reason = "every jobs boundary returns the crate's error enum by value"
    )]
    fn start(
        &self,
        arguments: &Map<String, Value>,
        extension: Option<ExtensionStart<'_>>,
    ) -> Result<Started, JobsError> {
        if let Some(error) = depth_refusal(self.depth, "delegate_spawn") {
            return Err(error);
        }
        if extension.is_some() {
            let unknown: Vec<_> = arguments
                .keys()
                .filter(|key| {
                    !matches!(
                        key.as_str(),
                        "description" | "prompt" | "model" | "timeout_ms"
                    )
                })
                .cloned()
                .collect();
            if !unknown.is_empty() {
                return Err(JobsError::ArgumentsNotAllowed { keys: unknown });
            }
        }
        let (description, prompt, timeout) = check_effects_arguments(arguments)?;
        // The schema leaves `model` optional and untyped, so a missing or
        // wrongly typed value reaches this check, which lists the valid
        // roles and references (`docs/delegates.md`, "Choosing a model").
        let model = match arguments.get("model") {
            Some(Value::String(model)) => model.clone(),
            None => {
                return Err(JobsError::ModelMissing {
                    catalogue: self.resolve.catalogue.to_string(),
                });
            }
            Some(_) => {
                return Err(JobsError::ModelNotString {
                    catalogue: self.resolve.catalogue.to_string(),
                });
            }
        };
        // Checked when the call is made, before any process starts: an
        // invalid value fails and lists the valid roles and references.
        let resolved = match (self.resolve.lookup)(&model) {
            Ok(resolved) => resolved,
            Err(reason) => {
                return Err(JobsError::ModelUnresolved {
                    reason,
                    catalogue: self.resolve.catalogue.to_string(),
                });
            }
        };
        // Checked after the model resolved and before anything starts:
        // a role naming a label the provider lacks fails with
        // `credential_missing`, naming the labels, and lists no
        // catalogue: a label is never added to it.
        let credential = match (self.resolve.credential)(&model) {
            Ok(credential) => credential,
            Err(message) => {
                return Err(JobsError::CredentialUnavailable { message });
            }
        };
        self.starter().start(
            "delegate_spawn",
            Origin::Spawn {
                resolved,
                credential,
            },
            description,
            prompt,
            timeout,
            extension,
        )
    }
}

/// What the shared start records: a spawn's resolved model and
/// credential, or a fork's resolved point and model reference. A spawn
/// resolves its model argument; a fork carries its point instead, so a
/// fork never resolves and never carries a credential.
pub(crate) enum Origin {
    /// A spawn's resolved `provider/model[:level]` and credential label.
    Spawn {
        /// The resolved reference.
        resolved: String,
        /// The credential label, when the role names one.
        credential: Option<String>,
    },
    /// A fork's resolved point and model reference.
    Fork {
        /// The resolved point and its verbatim model reference.
        from: Forking,
    },
}

/// The fields a delegate start admits, spawns and records with: the
/// admit-to-record tail both delegate tools share. Built per call from
/// either tool's wired fields, which name the same registry, parent,
/// paths, clock, bounds and closures.
pub(crate) struct Starter<'a> {
    /// The session's jobs.
    pub registry: &'a Arc<Registry>,
    /// The parent session: the child's `--parent`.
    pub parent: &'a SessionId,
    /// The parent's workspace, which the child shares.
    pub workspace: &'a PathBuf,
    /// Resolves the child's `events.jsonl` from its session id.
    pub events_for: &'a EventsFor,
    /// The session clock.
    pub clock: &'a Arc<dyn contract::clock::Clock>,
    /// How long a stop waits before SIGKILL.
    pub bound: Duration,
    /// The output cap, in bytes of the child's `events.jsonl`.
    pub cap: u64,
    /// Builds the child's command.
    pub launch: &'a Launch,
    /// Watches the child's socket.
    pub watch: &'a Watch,
    /// This session's depth in the delegate tree.
    pub depth: u32,
}

impl Starter<'_> {
    /// The shared start, built from either tool's wired fields: the one
    /// 10-field literal both tools' `starter()` share, so the wiring is
    /// written once with two users.
    #[allow(
        clippy::too_many_arguments,
        reason = "one per wired field both delegate tools share"
    )]
    fn wired<'a>(
        registry: &'a Arc<Registry>,
        parent: &'a SessionId,
        workspace: &'a PathBuf,
        events_for: &'a EventsFor,
        clock: &'a Arc<dyn contract::clock::Clock>,
        bound: Duration,
        cap: u64,
        launch: &'a Launch,
        watch: &'a Watch,
        depth: u32,
    ) -> Starter<'a> {
        Starter {
            registry,
            parent,
            workspace,
            events_for,
            clock,
            bound,
            cap,
            launch,
            watch,
            depth,
        }
    }

    /// Admits, spawns and records one delegate: the cap check, the spawn
    /// and the record run with `start_gate` held, exactly as
    /// `DelegateSpawn::start` did before the fork shared it. `tool` is
    /// the calling tool's name, for the record; `origin` is what the
    /// child runs as.
    #[allow(
        clippy::result_large_err,
        reason = "every jobs boundary returns the crate's error enum by value"
    )]
    pub(crate) fn start(
        &self,
        tool: &str,
        origin: Origin,
        description: String,
        prompt: String,
        timeout: Option<Duration>,
        mut extension: Option<ExtensionStart<'_>>,
    ) -> Result<Started, JobsError> {
        // The launch's start, and the record's model and point: a spawn
        // resolves its argument and records the resolved reference; a
        // fork writes its point's reference verbatim and its point,
        // carrying no credential.
        let (start, delegate_model, forked_from) = match origin {
            Origin::Spawn {
                resolved,
                credential,
            } => {
                let delegate_model = format!("fiber:{resolved}");
                (
                    Start::Fresh(Fresh::Spawn {
                        model: resolved,
                        credential,
                    }),
                    delegate_model,
                    None,
                )
            }
            Origin::Fork { from } => {
                let delegate_model = from.model.clone();
                let forked_from = Some(from.point.clone());
                (
                    Start::Fresh(Fresh::Fork { from }),
                    delegate_model,
                    forked_from,
                )
            }
        };
        let admitted = self.registry.admit(
            extension.is_some(),
            #[allow(
                clippy::result_large_err,
                reason = "every jobs boundary returns the crate's error enum by value"
            )]
            || {
                if extension.is_some() && !self.registry.has_inbox() {
                    return Err(JobsError::NoInbox);
                }
                let job_id = mint_job_id();
                if let Some((_, bind)) = extension.as_mut() {
                    bind(&job_id);
                }
                let session_id = run::mint_session_id();
                let output_path = (self.events_for)(&session_id)
                    .to_string_lossy()
                    .into_owned();
                let launched = Launched {
                    depth: self.depth.saturating_add(1),
                    session_id: session_id.clone(),
                    job_id: job_id.clone(),
                    parent: self.parent.clone(),
                    prompt,
                    workspace: self.workspace.clone(),
                    start,
                };
                // The stamp covers building the runner, spawning and listing the
                // child, and recording the job. No clock is read without an
                // installed timing.
                let start = self
                    .registry
                    .timing()
                    .map(|timing| (timing, self.clock.now()));
                let (runner, stop) = Runner::new(
                    job_id.clone(),
                    session_id.clone(),
                    PathBuf::from(&output_path),
                    Arc::clone(self.clock),
                    self.bound,
                    self.cap,
                    Arc::clone(self.watch),
                    None,
                    timeout,
                    Some(self.registry.usage_forward()),
                );
                let (child, listing) = runner
                    .spawn(self.launch, &launched)
                    .map_err(|source| JobsError::SpawnFailed { source })?;
                let delegate = DelegateStarted {
                    job_id: job_id.clone(),
                    delegate_session_id: session_id.clone(),
                    harness: "fiber".to_owned(),
                    // The resolved reference, with any role resolved: a full
                    // reference argument records the same text as before
                    // (`docs/events.md`, "`delegate_started`"). A fork
                    // records its point's reference verbatim instead.
                    model: delegate_model,
                    workspace: self.workspace.to_string_lossy().into_owned(),
                    worktree: None,
                    forked_from,
                };
                let (started, finish) = if let Some((extension, _)) = extension {
                    let started = JobStarted {
                        job_id: job_id.clone(),
                        tool: None,
                        extension: Some(extension.to_owned()),
                        description: description.clone(),
                        output_path: output_path.clone(),
                    };
                    let finish = self.registry.open_extension_started(
                        job_id.clone(),
                        extension.to_owned(),
                        description,
                        output_path.clone(),
                        stop,
                        delegate.clone(),
                    );
                    (started, finish)
                } else {
                    self.registry.open_started(
                        job_id.clone(),
                        tool.to_owned(),
                        description,
                        output_path.clone(),
                        stop,
                        delegate.clone(),
                    )
                };
                std::thread::spawn(move || runner.drive(child, Some(listing), finish));
                Ok((
                    Started {
                        job_id,
                        output_path,
                        started,
                        delegate,
                    },
                    start,
                ))
            },
        );
        match admitted {
            Ok((output, start)) => {
                // Timing emission holds no start gate: its sink may enter the registry.
                if let Some((timing, started)) = start {
                    drop(timing(Op::JobsStart, None, Some(started)));
                }
                Ok(output)
            }
            Err(crate::registry::runs::Admit::Closed) => Err(JobsError::ExtensionStartsClosed),
            Err(crate::registry::runs::Admit::Full) => Err(JobsError::DelegatesFull),
            Err(crate::registry::runs::Admit::Start(error)) => Err(error),
        }
    }
}

impl DelegateSpawn {
    /// The shared start, built from this tool's wired fields.
    fn starter(&self) -> Starter<'_> {
        Starter::wired(
            &self.registry,
            &self.parent,
            &self.workspace,
            &self.events_for,
            &self.clock,
            self.bound,
            self.cap,
            &self.launch,
            &self.watch,
            self.depth,
        )
    }
}

impl DelegateFork {
    /// The shared start, built from this tool's wired fields.
    fn starter(&self) -> Starter<'_> {
        Starter::wired(
            &self.registry,
            &self.parent,
            &self.workspace,
            &self.events_for,
            &self.clock,
            self.bound,
            self.cap,
            &self.launch,
            &self.watch,
            self.depth,
        )
    }

    /// Runs the fork from its checked arguments and calling action:
    /// the depth check, then the argument check, then the point read,
    /// then the start both tools share.
    #[allow(
        clippy::result_large_err,
        reason = "every jobs boundary returns the crate's error enum by value"
    )]
    fn start(
        &self,
        arguments: &Map<String, Value>,
        action: Option<ActionId>,
    ) -> Result<Started, JobsError> {
        if let Some(error) = depth_refusal(self.depth, "delegate_fork") {
            return Err(error);
        }
        let (description, prompt, timeout) = check_effects_arguments(arguments)?;
        // Read after the argument check and before admission: a bad call
        // never resolves a point, and a point failure starts nothing.
        let from =
            (self.point)(action.as_ref()).map_err(|failure| JobsError::ForkPoint { failure })?;
        self.starter().start(
            "delegate_fork",
            Origin::Fork { from },
            description,
            prompt,
            timeout,
            None,
        )
    }
}

#[cfg(test)]
#[path = "delegate/delegate_tests.rs"]
mod tests;

/// The rig the delegate tool tests share: a registry on a fake clock
/// with an inbox tap and a sessions dir, and a launcher that records
/// its `Launched` and starts a `sleep` child the test finds through its
/// pidfile, with a refusing watch. `point_calls` counts the point reads
/// and `seen_actions` records the calling actions: the fork tests'
/// counters, which the spawn tests leave alone.
#[cfg(test)]
pub(crate) mod test_rig {
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use contract::inbox::Delivery;
    use fakes::clock::FakeClock;
    use fakes::{Deadline, Recorder, TempDir, Watchdog, within};

    use super::run::{Launch, Launched, Watch};
    use crate::registry::Registry;
    use contract::jobs::Jobs as _;

    /// How long a test waits on the wall clock before it fails.
    pub(crate) const DEADLINE: Duration = Duration::from_secs(3);

    pub(crate) struct Rig {
        pub _dir: TempDir,
        pub registry: Arc<Registry>,
        pub clock: Arc<FakeClock>,
        pub inbox: mpsc::Receiver<Delivery>,
        pub sessions: std::path::PathBuf,
        pub launched: mpsc::Receiver<Launched>,
        pub launch_count: Arc<AtomicUsize>,
        pub point_calls: Arc<AtomicUsize>,
        pub seen_actions: Arc<std::sync::Mutex<Vec<Option<contract::ActionId>>>>,
        pub launch: Launch,
        pub watch: Watch,
    }

    pub(crate) fn rig() -> Rig {
        let dir = TempDir::new("fiber-delegate");
        let clock = FakeClock::new();
        let artifacts = dir.path().join("artifacts");
        std::fs::create_dir(&artifacts).unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        let registry = Registry::new(
            artifacts,
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
            Arc::new(Recorder::default()),
        );
        let (inbox_tx, inbox_rx) = mpsc::channel();
        registry.deliver_to(inbox_tx);
        let (launched_tx, launched_rx) = mpsc::channel();
        let launch_count = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launch_count);
        let pid = dir.path().join("child.pid");
        let launch: Launch = Arc::new(move |launched: &Launched| {
            counted.fetch_add(1, Ordering::SeqCst);
            let _sent = launched_tx.send(Launched {
                depth: launched.depth,
                session_id: launched.session_id.clone(),
                job_id: launched.job_id.clone(),
                parent: launched.parent.clone(),
                prompt: launched.prompt.clone(),
                workspace: launched.workspace.clone(),
                start: launched.start.clone(),
            });
            let mut command = Command::new("sh");
            command.args([
                "-c",
                &format!("echo $$ > '{}'; exec sleep 60", pid.display()),
            ]);
            command
        });
        let watch: Watch = Arc::new(|_, _, _| Err(std::io::Error::other("refused")));
        Rig {
            _dir: dir,
            registry,
            clock,
            inbox: inbox_rx,
            sessions,
            launched: launched_rx,
            launch_count,
            point_calls: Arc::new(AtomicUsize::new(0)),
            seen_actions: Arc::new(std::sync::Mutex::new(Vec::new())),
            launch,
            watch,
        }
    }

    /// The `sleep` the rig's launcher started, read from its pidfile:
    /// armed as its watchdog so a failing test leaves no child behind.
    #[track_caller]
    pub(crate) fn child_group(rig: &Rig) -> Watchdog {
        let pid = rig._dir.path().join("child.pid");
        let pid: u32 = within(
            "the child writes its pid",
            Deadline::after(DEADLINE),
            move || {
                loop {
                    let Ok(text) = std::fs::read_to_string(&pid) else {
                        std::thread::yield_now();
                        continue;
                    };
                    let Ok(pid) = text.trim().parse() else {
                        std::thread::yield_now();
                        continue;
                    };
                    return pid;
                }
            },
        );
        Watchdog::group(pid)
    }
}
