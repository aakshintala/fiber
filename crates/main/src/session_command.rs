//! The internal session command (`docs/invocation.md`, "Commands and
//! flags" and "Processes"), and the one function that builds and runs
//! every new session, `fiber ask`'s included.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::diag::DebugLog;
use contract::events::{Parent, VariablesSource};
use contract::shapes::{Failure, Worktree};
use contract::{ErrorCode, SessionId};
use doors::Session;
use log::Log;
use r#loop::Loop;

use crate::{
    Parts, ask_failed, ask_permissions, cli, failed, finish, late_emit, mcp_servers,
    parts_with_session, run_turn, session_extensions, shutdown,
};

thread_local! {
    // debt: the opener reaches run_new_with through a thread-local so run_new keeps its signature for rewind.rs. Upgrade trigger: when rewind.rs passes the opener explicitly, pass it as an argument and delete OPENED and OpenedHold.
    /// The worktree's create-line opener for the `run_new` below.
    /// `run_new_with` holds it here while `run_new` runs, so the session
    /// directory's `events.jsonl` join stays in `fn run_new`: the context the
    /// session-path allowance names, while `run_new` keeps the signature
    /// `rewind.rs` calls.
    static OPENED: RefCell<Option<doors::WorktreeOpener>> = RefCell::new(None);
}

/// Holds a worktree opener for `run_new`: set on entry, the previous
/// value back on drop, so a panic still leaves the slot as it was.
struct OpenedHold {
    previous: Option<doors::WorktreeOpener>,
}

impl OpenedHold {
    fn set(opened: Option<doors::WorktreeOpener>) -> Self {
        let previous = OPENED.with(|cell| cell.replace(opened));
        Self { previous }
    }
}

impl Drop for OpenedHold {
    fn drop(&mut self) {
        OPENED.with(|cell| cell.replace(self.previous.take()));
    }
}

pub(crate) struct CaseInputs {
    case_run: Arc<extensions::case::CaseRun>,
    host: Arc<extensions::HostScript>,
}

pub(crate) enum WorkspaceStart {
    Direct,
    CreatedWorktree {
        worktree: Worktree,
        opened: doors::WorktreeOpener,
    },
    ExistingWorktree {
        worktree: Worktree,
    },
}

impl WorkspaceStart {
    fn new(
        worktree: Option<Worktree>,
        opened: Option<doors::WorktreeOpener>,
    ) -> Result<Self, Failure> {
        match (worktree, opened) {
            (None, None) => Ok(Self::Direct),
            (Some(worktree), Some(opened)) => Ok(Self::CreatedWorktree { worktree, opened }),
            (Some(worktree), None) => Ok(Self::ExistingWorktree { worktree }),
            (None, Some(_)) => Err(failed(
                ErrorCode::IoFailed,
                "a worktree opener has no worktree",
            )),
        }
    }

    pub(crate) fn worktree(&self) -> Option<&Worktree> {
        match self {
            Self::Direct => None,
            Self::CreatedWorktree { worktree, .. } | Self::ExistingWorktree { worktree } => {
                Some(worktree)
            }
        }
    }

    pub(crate) fn opened(&self) -> Option<&doors::WorktreeOpener> {
        match self {
            Self::CreatedWorktree { opened, .. } => Some(opened),
            Self::Direct | Self::ExistingWorktree { .. } => None,
        }
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "resume carries its log, fold and directory for assembly"
)]
pub(crate) enum Start {
    New {
        id: SessionId,
        workspace: WorkspaceStart,
        supervised: bool,
        case_inputs: Option<CaseInputs>,
        inherited: Vec<PathBuf>,
    },
    Delegate {
        id: SessionId,
        parent: Parent,
        workspace: WorkspaceStart,
        inherited: Vec<PathBuf>,
    },
    Rewound {
        id: SessionId,
        continuation: r#loop::Rewound,
        workspace: WorkspaceStart,
        inherited: Vec<PathBuf>,
    },
    Resume(ResumeStart),
}

pub(crate) struct ResumeStart {
    pub(crate) dir: log::SessionDir,
    pub(crate) log: Arc<Log>,
    pub(crate) folded: r#loop::Resumed,
    pub(crate) opened: Option<doors::WorktreeOpener>,
}

impl ResumeStart {
    pub(crate) fn new(
        dir: log::SessionDir,
        log: Arc<Log>,
        folded: r#loop::Resumed,
        opened: Option<doors::WorktreeOpener>,
    ) -> Result<Self, Failure> {
        if dir.path() != log.dir() {
            return Err(failed(
                ErrorCode::IoFailed,
                "The resumed log does not match the session directory.",
            ));
        }
        Ok(Self {
            dir,
            log,
            folded,
            opened,
        })
    }

    pub(crate) fn id(&self) -> SessionId {
        let name = self
            .dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        SessionId(name.to_owned())
    }

    pub(crate) fn sessions(&self) -> Result<&Path, Failure> {
        self.dir
            .path()
            .parent()
            .ok_or_else(|| failed(ErrorCode::IoFailed, "the session directory has no parent"))
    }
}

pub(crate) struct SessionInputs {
    pub(crate) prompt: Option<String>,
    pub(crate) one_turn: bool,
    pub(crate) start: Start,
    pub(crate) fiber_path: PathBuf,
    pub(crate) reads: Arc<crate::switch::Reads>,
}

/// Whether the session may answer an approval: the base run's
/// answerability, and never when a program drives the session
/// (`docs/permissions.md`, "Supervised sessions"). Both the fresh and
/// the resumed path compute their effective value through here, so a
/// supervised session stays unanswerable however it started.
pub(crate) fn effective_answerable(base: bool, supervised: bool) -> bool {
    base && !supervised
}

/// The loop's turn-start command wiring, shared by the new-session and
/// resume construction paths: the refresh reads the current extension
/// commands and replaces the session's rows (the same complete recompute
/// the construction above builds with, after every reload too), while the
/// sync keeps the reload baseline the next turn-start check measures
/// against the installed answer.
pub(crate) fn command_update_wiring(
    current: &Arc<crate::session_extensions::Current>,
    session: &Session,
    skill_set: &r#loop::SkillSet,
) -> (r#loop::CommandsRefresh, crate::reload::CommandsSync) {
    // The loop's turn-start refresh reads the current extensions and
    // replaces the session's rows: the same complete recompute the
    // construction above builds with, after every reload too.
    let commands_refresh = r#loop::CommandsRefresh {
        extensions: Arc::new({
            let current = Arc::clone(current);
            move || current.get().commands()
        }),
        replace: session.commands_replacer(),
    };
    // A reload's install syncs the same baseline its rows replace, so
    // the next turn-start check measures against the installed answer.
    let commands_sync: crate::reload::CommandsSync = {
        let skill_set = skill_set.clone();
        Arc::new(move |runnable: &r#loop::RunnableCommands| skill_set.sync_commands(runnable))
    };
    (commands_refresh, commands_sync)
}

/// The internal session command: one session process bound at
/// `run/<session_id>` (`docs/invocation.md`, "Processes"). The workspace
/// is entered before signals are installed or any thread starts, so the
/// shared path reads it as the current directory exactly as `ask` does.
/// With `--worktree` it moves once more, into the new worktree, after
/// that: the signals' bound thread reads no relative path. With `--resume`
/// the workspace is the launch directory the hub selected, and the session
/// enters the recorded worktree itself instead of starting one. Stdin is never read.
pub(crate) fn run(args: cli::SessionArgs, process: &crate::startup::Process) -> i32 {
    let clock = Arc::clone(&process.clock);
    crate::startup_log::as_session();
    if let Err(e) = std::env::set_current_dir(&args.workspace) {
        // The hub spawns a continues session with the recorded worktree
        // as the workspace: a path removed after validation means the
        // old session ended and cleaned up, so an absent path names the
        // old session missing (Rulings 15). A present path keeps the
        // `io_failed`, as today.
        if let Some(old) = args.continues.as_ref().filter(|_| !args.workspace.is_dir()) {
            return ask_failed(failed(
                ErrorCode::SessionNotFound,
                format!("No session `{old}`."),
            ));
        }
        return ask_failed(failed(
            ErrorCode::IoFailed,
            format!("{}: {e}", args.workspace.display()),
        ));
    }
    // As `ask`: a signal while starting exits at once.
    let signals = match crate::shutdown::install(Arc::clone(&clock)) {
        Ok(signals) => signals,
        Err(e) => return ask_failed(failed(ErrorCode::IoFailed, format!("signals: {e}"))),
    };
    // The hub passes `--login-shell` when the session's environment came
    // from its login-shell capture: every entry below records it, and a
    // delegate the session starts carries it (`docs/invocation.md`, "A
    // session's environment").
    crate::delegates::set_depth(args.depth.unwrap_or(0));
    if args.login_shell {
        r#loop::set_variables_source(VariablesSource::LoginShell);
    }
    if args.resume {
        // A delegate's resume runs its prompt as one unattended turn,
        // then exits, as a new delegate does: stdin is its lifeline.
        if args.parent.is_some() {
            signals.lifeline(Box::new(std::io::stdin()));
        }
        return crate::resume::session_resume(
            SessionId(args.id),
            crate::per_run(args.model, args.overrides),
            args.credential,
            args.prompt,
            clock,
            &signals,
            process,
        );
    }
    if let Some(from) = args.rewound_from {
        return crate::rewind::session_rewound(
            SessionId(args.id),
            SessionId(from),
            clock,
            &signals,
            process,
        );
    }
    if let Some(old) = args.continues {
        // The original spelling rides along for the identity compare:
        // `current_dir` above already normalized slashes and symlinks
        // away, while the hub enforced the exact string. A `continues`
        // start is a new session: only the new request's `supervised`
        // applies, never the old session's.
        return crate::continue_session::session_continues(
            SessionId(args.id),
            SessionId(old),
            crate::per_run(args.model, args.overrides),
            &args.workspace,
            clock,
            &signals,
            process,
            args.supervised,
        );
    }
    // Every delegate adopts its own worktree before writing session_started.
    // The share stays alive through both fork and fresh-session teardown.
    let adopted = if args.parent.is_some() && args.worktree {
        match doors::adopt_delegate_worktree(&args.workspace, &SessionId(args.id.clone())) {
            Ok(adopted) => adopted,
            Err(failure) => return report(&signals, Failure::from(failure)),
        }
    } else {
        None
    };
    let (adopted, _shared) = adopted
        .map(|(worktree, shared)| (Some(worktree), Some(shared)))
        .unwrap_or((None, None));
    // A fork names its point outright: no last line is checked. The
    // parent, the delegate job and the prompt ride with the flags.
    if args.forked_from.is_some() {
        let (Some(from), Some(seq), Some(parent), Some(prompt)) = (
            args.forked_from,
            args.forked_seq,
            args.parent
                .zip(args.delegate_id)
                .map(|(session_id, delegate_id)| Parent {
                    session_id: SessionId(session_id),
                    delegate_id: contract::JobId(delegate_id),
                }),
            args.prompt,
        ) else {
            return ask_failed(failed(
                ErrorCode::Usage,
                "--forked-from requires --forked-seq, --parent, --delegate-id and --prompt",
            ));
        };
        return crate::rewind::session_forked(
            SessionId(args.id),
            contract::shapes::Point {
                session_id: SessionId(from),
                seq: contract::Seq(seq),
                project: None,
            },
            parent,
            args.depth.unwrap_or(0),
            prompt,
            args.added,
            adopted,
            clock,
            &signals,
            process,
        );
    }
    // A delegate's stdin is its lifeline: the parent holds the write end
    // open and never writes to it (`docs/delegates.md`, "Lifetime").
    let parent = args
        .parent
        .zip(args.delegate_id)
        .map(|(session_id, delegate_id)| Parent {
            session_id: SessionId(session_id),
            delegate_id: contract::JobId(delegate_id),
        });
    if parent.is_some() {
        signals.lifeline(Box::new(std::io::stdin()));
    }
    // The child session command carries the delegate's launch-time
    // folders: a fresh spawn marks them inherited, while a fork records
    // only the roots missing from its point-based history.
    new_session(
        SessionId(args.id),
        crate::per_run(args.model, args.overrides),
        args.prompt,
        parent.is_some(),
        args.worktree,
        adopted,
        clock,
        &signals,
        process,
        None,
        parent,
        args.supervised,
        args.added.clone(),
    )
}

/// One function prepares every new session (`docs/invocation.md`,
/// "Processes"): `fiber ask` and the internal session command differ
/// only in the id's source (minted vs `--id`), the workspace (the current
/// directory on entry, applied by chdir before the call) and the first
/// deliveries (prompt plus `close` vs an optional prompt). With `worktree`
/// the process arms, creates the worktree and enters it before the session
/// runs, then ends the worktree however the run went. Case sessions share
/// their clock and host script with extension setup and execution.
#[allow(
    clippy::too_many_arguments,
    reason = "session preparation carries worktree and case inputs to the shared runner"
)]
pub(crate) fn new_session(
    id: SessionId,
    overrides: Vec<String>,
    prompt: Option<String>,
    one_turn: bool,
    worktree: bool,
    adopted: Option<Worktree>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
    case_run: Option<Arc<extensions::case::CaseRun>>,
    parent: Option<Parent>,
    supervised: bool,
    inherited: Vec<PathBuf>,
) -> i32 {
    // One reads for the process: the worktree's `git worktree add` and
    // every startup credential read run through it, so one cancel reaches
    // them all. The guard drops after the worktree's isolation ends, and
    // cancels on every return, whatever the result.
    let reads = Arc::new(crate::switch::Reads::default());
    let _cancel = crate::switch::CancelOnDrop(Arc::clone(&reads));
    let clock = case_run.as_ref().map_or(clock, |case| case.session_clock());
    let case_inputs = case_run.map(|case| CaseInputs {
        host: case.host_script(),
        case_run: case,
    });
    if !worktree || adopted.is_some() {
        return match run_new(
            id,
            overrides,
            prompt,
            one_turn,
            adopted,
            clock,
            signals,
            process,
            case_inputs,
            parent,
            None,
            reads,
            supervised,
            inherited,
        ) {
            Ok(code) => code,
            Err(failure) => report(signals, failure),
        };
    }
    // As `parts_with` would: a failure here leaves no session.
    let (home, launch) = match crate::startup::startup() {
        Ok(directories) => directories,
        Err(e) => return ask_failed(e),
    };
    // Its own reads: the worktree's `git worktree add` runs through them,
    // so a signal kills git and its hook together.
    shutdown::arm(signals, &reads);
    let isolation = match doors::isolate(&launch, &home, &id, Arc::clone(&clock), &|command| {
        reads.command(command)
    }) {
        Ok(isolation) => isolation,
        Err(failure) => return report(signals, Failure::from(failure)),
    };
    if let Err(e) = std::env::set_current_dir(isolation.path()) {
        let failure = failed(
            ErrorCode::IoFailed,
            format!("{}: {e}", isolation.path().display()),
        );
        isolation.end();
        return report(signals, failure);
    }
    let worktree = isolation.worktree();
    let opened = isolation.opener();
    let result = run_new_with(
        id,
        overrides,
        prompt,
        one_turn,
        Some(worktree),
        clock,
        signals,
        process,
        case_inputs,
        parent,
        None,
        reads,
        Some(opened),
        supervised,
        inherited,
    );
    isolation.end();
    match result {
        Ok(code) => code,
        Err(failure) => report(signals, failure),
    }
}

/// Builds and runs the session with an optional worktree. Every `Err` is
/// reported by [`report`], after the worktree ends when one was created.
#[allow(
    clippy::too_many_arguments,
    reason = "session execution carries its isolation, case and startup inputs"
)]
pub(crate) fn run_new(
    id: SessionId,
    overrides: Vec<String>,
    prompt: Option<String>,
    one_turn: bool,
    worktree: Option<Worktree>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
    case_inputs: Option<CaseInputs>,
    parent: Option<Parent>,
    rewound: Option<r#loop::Rewound>,
    reads: Arc<crate::switch::Reads>,
    supervised: bool,
    inherited: Vec<PathBuf>,
) -> Result<i32, Failure> {
    // Held by `run_new_with` for a fresh worktree, empty on every direct
    // call: taking leaves the slot empty, so a panic leaves nothing
    // behind either.
    let opened: Option<doors::WorktreeOpener> = OPENED.with(|cell| cell.take());
    // Armed before the parts are read: from now a signal cancels the one
    // reads every startup credential command runs through.
    shutdown::arm(signals, &reads);
    let fiber_path = process.exe.clone()?;
    // A rewind's new session runs where the old one did, with the model,
    // credential and thinking level the old log's latest build recorded,
    // as a resume keeps its recorded ones (`docs/events.md`, "Rewind").
    // The diagnostic process: a one-turn run without a rewind or a parent
    // is an ask; a delegate child runs one turn but keeps the session's
    // identity, as does a rewind continuing its session's log.
    let diag_process = if one_turn && rewound.is_none() && parent.is_none() {
        log::diag::Process::Ask
    } else {
        log::diag::Process::Session
    };
    let parts = match &rewound {
        Some(start) => {
            let (home, workspace) = crate::startup::startup()?;
            let sessions = log::sessions_dir(&home, &worktree::project(&workspace));
            // The point's session lives beside the child, or in the
            // recorded parent project when the continuation names one: a
            // workspace move, or a rewind to a point in another project.
            // The source sessions directory, never the target's.
            let from_sessions = match &start.from.project {
                Some(key) => log::sessions_dir_for_key(&home, key).ok_or_else(|| {
                    failed(
                        ErrorCode::LogCorrupt,
                        format!(
                            "The continuation names a parent project `{key}` that is not one path component."
                        ),
                    )
                })?,
                None => sessions.clone(),
            };
            let from = log::SessionDir::new(&from_sessions, &start.from.session_id);
            let folded =
                r#loop::forked(from.path(), start.from.seq).map_err(|e| failed(e.code(), e))?;
            let thinking = folded
                .thinking
                .as_deref()
                .and_then(|level| level.parse::<contract::ThinkingLevel>().ok());
            let segments = log::history_to(from.path(), start.from.seq)
                .map_err(|error| failed(error.code(), error))?;
            // The fresh terminal has no child on disk yet, so its kind
            // comes from the continuation: a fork folds `Latest` as of the
            // point, a rewind at the parent's end.
            let seed = match &start.how {
                r#loop::Continuation::Fork { .. } => crate::resume::fork_seed(&sessions, &segments),
                r#loop::Continuation::Rewind { .. } => {
                    crate::resume::chain_seed(&sessions, &segments)
                }
            }
            .map_err(|error| failed(error.code(), error))?;
            parts_with_session(
                Vec::new(),
                folded.model.as_deref(),
                folded.credential.as_deref(),
                thinking,
                Arc::clone(&clock),
                case_inputs.as_ref().map(|inputs| Arc::clone(&inputs.host)),
                Arc::clone(&reads),
                diag_process,
                seed,
                crate::session_extensions::RepositoryScope::default(),
                crate::parts::SessionLaunch {
                    id: id.clone(),
                    reason: match &start.how {
                        r#loop::Continuation::Fork { .. } => {
                            contract::events::ExtensionReason::Fork
                        }
                        r#loop::Continuation::Rewind { .. } => {
                            contract::events::ExtensionReason::Rewind
                        }
                    },
                    event_cursor: 0,
                },
                fiber_path.clone(),
            )?
        }
        None => {
            let overrides_for_fix = overrides.clone();
            parts_with_session(
                overrides,
                None,
                None,
                None,
                Arc::clone(&clock),
                case_inputs.as_ref().map(|inputs| Arc::clone(&inputs.host)),
                Arc::clone(&reads),
                diag_process,
                extensions::StateSeed::new(),
                crate::session_extensions::RepositoryScope::default(),
                crate::parts::SessionLaunch {
                    id: id.clone(),
                    reason: contract::events::ExtensionReason::New,
                    event_cursor: 0,
                },
                fiber_path.clone(),
            )
            .map_err(|failure| ::cli::with_fix(failure, &overrides_for_fix))?
        }
    };
    let workspace_start = WorkspaceStart::new(worktree, opened)?;
    let start = match (parent, rewound) {
        (Some(parent), None) => Start::Delegate {
            id: id.clone(),
            parent,
            workspace: workspace_start,
            inherited,
        },
        (_, Some(continuation)) => Start::Rewound {
            id: id.clone(),
            continuation,
            workspace: workspace_start,
            inherited,
        },
        (None, None) => Start::New {
            id: id.clone(),
            workspace: workspace_start,
            supervised,
            case_inputs,
            inherited,
        },
    };
    let inputs = SessionInputs {
        prompt,
        one_turn,
        start,
        fiber_path: fiber_path.clone(),
        reads,
    };
    assemble_session(parts, inputs, signals, process)
}

/// One function assembles every session after its [`Parts`] are built:
/// the single exhaustive [`Start`] dispatch runs the new, delegate and
/// rewound paths (which share one body) or the resumed path.
pub(crate) fn assemble_session(
    mut parts: Parts,
    inputs: SessionInputs,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
) -> Result<i32, Failure> {
    let SessionInputs {
        prompt,
        one_turn,
        start,
        fiber_path,
        reads,
    } = inputs;
    match start {
        Start::Resume(resume) => {
            let id = resume.id();
            let sessions_buf = resume.sessions()?.to_path_buf();
            let ResumeStart {
                dir,
                log,
                folded,
                opened,
            } = resume;
            let clock = Arc::clone(&process.clock);
            // The session directory's log: the opening message's
            // environment names it.
            parts.prompt.session_log = dir.events().display().to_string();
            let Parts {
                sources,
                provider,
                observe,
                reviewer_observe,
                diag,
                model,
                prompt: prompt_inputs,
                reviewer,
                limits,
                reviewer_notes,
                budget,
                retry,
                handoff,
                idle,
                warm,
                caps,
                home,
                project,
                credential_files,
                extensions,
                locks,
                mut mcp,
                pending_mcp,
                switching,
                switchable,
                resolve,
                web_search,
                shell_read_only,
                sandbox,
                protection,
                repository,
                ..
            } = parts;
            // The log is already open: install the writer before any append the
            // resumed process makes. The level is checked once; its answer gates
            // both the install and the connection timing below.
            let timing = if diag.debugging() {
                log.install_timing(Arc::clone(&diag));
                Some(diag.timings())
            } else {
                None
            };
            // The recreated worktree's create line opens here, before the
            // session file attaches: a kept present worktree has no opener, so
            // it writes none.
            if let (Some(timing), Some(opened)) = (&timing, &opened) {
                opened(timing);
            }
            // The recorded workspace, not the launch directory. A failure here,
            // such as not finding the running binary, returns before `fiber_started`,
            // so the log stays as it was. Every configured stdio server starts too.
            // Jobs live and die with the process: a resumed session's registry
            // starts empty (`docs/tools.md`, "Background jobs").
            let jobs = jobs::Registry::new(
                dir.artifacts(),
                Arc::clone(&clock),
                Arc::new(log::WeakEmit::new(&log)) as _,
            );
            // The registry starts empty on a resumed session too: installing the
            // session's timing times a background start as `jobs.start`.
            if let Some(timing) = &timing {
                jobs.install_timing(Arc::clone(timing));
            }
            // A resumed session declares `delegate_spawn` too, parented to itself.
            let crate::switch::quota::QuotaReaders {
                door: quota,
                model: model_quota,
                notice,
            } = switching.quota_readers();
            let delegates = crate::delegates::Delegates::new(
                fiber_path.clone(),
                home.clone(),
                id.clone(),
                Path::new(&folded.workspace).to_path_buf(),
                sessions_buf,
                Arc::clone(&jobs),
                Arc::clone(&clock),
                resolve,
            )
            .with_quota(model_quota);
            crate::shutdown::arm(signals, &reads);
            let skill_set =
                r#loop::SkillSet::new(prompt_inputs.clone(), Path::new(&folded.workspace));
            let skills: Arc<dyn contract::skills::Skills> = Arc::new(skill_set.reader());
            let web_search = crate::builtin::web_search(
                web_search.as_deref(),
                extensions.search_backend().as_ref(),
            )?;
            let pending_mcp = match pending_mcp {
                Some(pending) => pending,
                None => {
                    let _ = extensions.finish_processes(&support::cancel::Token::new());
                    return Err(failed(ErrorCode::IoFailed, "MCP startup was not prepared"));
                }
            };
            // The session's sink observes each `web_fetch` request at debug; at
            // any other level the fetch stays silent.
            let debug = diag
                .debugging()
                .then(|| Arc::clone(&diag) as Arc<dyn DebugLog>);
            let (tools, infos, driver, session_servers) = match crate::mcp_servers::session_tools(
                fiber_path.clone(),
                &home,
                Path::new(&folded.workspace),
                &dir.artifacts(),
                &clock,
                &jobs,
                &locks,
                &project,
                pending_mcp,
                web_search,
                &delegates,
                skills,
                extensions.tools(),
                timing.clone(),
                debug,
                shell_read_only.clone(),
                sandbox.clone(),
                Some(protection),
            ) {
                Ok(tools) => tools,
                Err(failure) => {
                    let _ = extensions.finish_processes(&support::cancel::Token::new());
                    return Err(failure);
                }
            };
            let delegates = std::sync::Arc::new(delegates);
            let forget = Arc::clone(&session_servers.forget);
            let hosted_stands = crate::switch::hosted_stands(&tools);
            let workspace = std::path::PathBuf::from(&folded.workspace);
            let offer = Arc::new(extensions::SessionOffer::new(
                &home,
                &project,
                &workspace,
                crate::session_extensions::fetch(&clock),
            ));
            let permissions = crate::ask_permissions(
                &home,
                &project,
                folded.workspace.clone(),
                credential_files,
                &clock,
            );
            // The durable log is held: the diagnostic writer takes the session's
            // file and id, so the door's socket timer and every later line share
            // them, while MCP and permission lines keep the pre-attach file.
            diag.attach(&id);
            let session = match Session::resume_with_timing(
                &home,
                dir.path(),
                &log,
                Arc::clone(&clock),
                infos,
                Box::new(io::stdout()),
                timing,
            ) {
                Ok(session) => session,
                Err(e) => {
                    session_servers.servers.stop();
                    return Err(Failure::from(e));
                }
            };
            session.shell(driver);
            session.notes_workspace(workspace.clone());
            session.jobs(jobs.clone());
            session.repository_code(
                Arc::clone(&offer) as Arc<dyn contract::repository::RepositoryCode>
            );
            session.images(Arc::clone(&session_servers.images));
            session.hooks(Arc::clone(&extensions) as Arc<dyn contract::hook::Hooks>);
            session
                .extensions(Arc::clone(&extensions) as Arc<dyn contract::extension::ExtensionDoor>);
            extensions.emit_to(Arc::new(log::WeakEmit::new(&log)));
            extensions.drive_to(session.driver());
            let switching = Arc::new(switching.diagnostics(Arc::clone(&diag)));
            let host_model = Arc::new(crate::model_host::ModelHost::new(
                switching.reacher(Arc::new(crate::connect)),
                budget,
                &retry,
                dir.path().to_path_buf(),
                id.clone(),
            ));
            extensions.model_to(host_model.clone());
            extensions
                .delegate_to(Arc::clone(&delegates) as Arc<dyn contract::extension::HostDelegate>);
            // A resume restores the flag from the log's first line: a supervised
            // resume stays unanswerable even when a person drives it
            // (`docs/permissions.md`, "Supervised sessions").
            let supervised = folded.supervised;
            let answerable = effective_answerable(!one_turn, supervised);
            extensions.answerable(answerable);
            extensions.browse_with(
                Arc::new(extensions::SystemBrowser::default()),
                session.attended(),
            );
            session.skills(r#loop::skills(&prompt_inputs, Path::new(&folded.workspace)));
            // The session's MCP prompt rows, as a new session lists them: the
            // `commands` answer lists them beside skills, and `/name` runs them
            // through the fetch below (`docs/mcp.md`, "Prompts and resources").
            // Shadowed prompts join the startup notices.
            let prompt_rows = session_servers.prompts.commands();
            let listed = r#loop::commands(
                &prompt_inputs,
                Path::new(&folded.workspace),
                &prompt_rows,
                &extensions.commands(),
            );
            mcp.notices.extend(listed.notices);
            // The turn-start check measures the runnable set against these rows,
            // so a template added while the session was away still refreshes them.
            skill_set.sync_commands(&listed.runnable);
            session.commands(listed.rows);
            session.quota(quota);
            let door = crate::switch::Door {
                declare: session.declarer(),
                hosted_stands,
                backend: extensions.search_backend(),
            };
            let current = Arc::new(crate::session_extensions::Current::new(extensions));
            let (commands_refresh, commands_sync) =
                command_update_wiring(&current, &session, &skill_set);
            let readers = switching
                .credential_readers(switching.clone().closure(door), model.reference.clone());
            session.credentials(readers.labels, readers.admitted);
            let switcher = readers.prepare;
            let (reload, load, servers, prompts, server_failures) = crate::reload::State::new(
                sources,
                prompt_inputs.clone(),
                current.clone(),
                switching.clone(),
                &tools,
                &session,
                host_model,
                Arc::clone(&delegates) as Arc<dyn contract::extension::HostDelegate>,
                session_servers,
                answerable,
                log.clone(),
                diag.clone(),
                shell_read_only,
                sandbox,
                std::sync::Arc::new(std::sync::Mutex::new(repository)),
                commands_sync,
            );
            // Every notice the startup already wrote, including the ones
            // `mcp_servers_started` writes after this read: the late preparer
            // drops the ones it would repeat and keeps the new ones.
            let written = crate::session_extensions::startup_written(&current.get(), &mcp.notices);
            let providers = reload.mcp_providers(&server_failures);
            let (reloader, late) = reload.preparers(written);
            session.mcp_servers(providers.refresh);
            session.mcp_login(providers.login);
            session.personal_source_check(crate::prompt_files::personal_source_check(
                prompt_inputs.home.clone(),
                prompt_inputs.agents_home.clone(),
                Arc::clone(&prompt_inputs.clock),
            ));
            let cancel = Arc::new(r#loop::TurnCancel::default());
            // A signal while armed: the log stays as it was. The commit below
            // decides the window between `start` and the first write: a recorded
            // signal writes nothing and exits with its code.
            let reads = switching.reads();
            if let Some(code) =
                crate::shutdown::start(signals, &cancel, &session, jobs.clone(), reads)
            {
                servers.stop();
                close(session, log, &home, dir.path(), &workspace, &*clock);
                return Ok(code);
            }
            match signals.commit(|| {
                r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), true)
                    .and_then(|()| crate::session_extensions::written(&log, &current.get()))
                    .and_then(|()| {
                        // The providers load reports the same shadow the
                        // session's load reported: `written` wrote that half.
                        let notices =
                            crate::session_extensions::unwritten(mcp.notices, &current.get());
                        r#loop::mcp_servers_started(&log, server_failures, notices)
                    })
            }) {
                Err(code) => {
                    servers.stop();
                    close(session, log, &home, dir.path(), &workspace, &*clock);
                    return Ok(code);
                }
                Ok(Err(e)) => {
                    servers.stop();
                    close(session, log, &home, dir.path(), &workspace, &*clock);
                    return Err(failed(e.code(), e));
                }
                Ok(Ok(())) => {}
            }
            // The session file and id are already attached above, before the
            // door opened: request, extension and memory lines share them.
            let inbox_wake = session.inbox_wake();
            let rewrites = session.rewrites();
            let admission = session.admission();
            let steering = session.steering();
            // The fetch runs a prompt row through its server, starting a lazy
            // one as a first tool call does (`docs/mcp.md`, "Prompts and
            // resources"). No logic lives here beyond that call.
            let fetch_prompts = prompts.clone();
            let fetch: r#loop::FetchPrompt = Arc::new(move |server, prompt, text, cancel| {
                fetch_prompts.get(server, prompt, text, cancel)
            });
            let code = run_turn(
                &session,
                &log,
                dir.path(),
                prompt,
                one_turn,
                cancel,
                &diag,
                &current,
                |inbox, cancel, slot| {
                    // A resume that fails leaves no loop to take the summary from:
                    // the earlier process's open requests still reach the exit.
                    *slot = Some(folded.exit_seed());
                    crate::finish(
                        Loop::resume(
                            r#loop::Session {
                                log: Arc::clone(&log),
                                provider,
                                model,
                                prompt: prompt_inputs,
                                inbox,
                                tools: r#loop::capped(tools, &caps),
                                permissions,
                            },
                            folded,
                        )
                        .map(|looped| {
                            let looped = crate::session_extensions::hooked(
                                looped.jobs(jobs).quota(notice),
                                &current,
                            );
                            let looped =
                                crate::session_extensions::released(looped, Arc::clone(&current));
                            looped
                                .diagnostics(Arc::clone(&diag))
                                .observed(observe)
                                .reviewer_observed(reviewer_observe)
                                .reviewer_notes(reviewer_notes)
                                .handoff(handoff)
                                .on_handoff(forget)
                                .switcher(switcher, switchable)
                                .reloader(reloader)
                                .reconnector(providers.reconnect)
                                .late_extensions(late)
                                .repository_code(offer)
                                .approved_code(load)
                                .skill_set(skill_set)
                                .commands_refresh(commands_refresh)
                                .server_prompts(r#loop::ServerPrompts {
                                    rows: prompt_rows,
                                    fetch,
                                })
                                .inbox_wake(inbox_wake)
                                .on_rewrite(rewrites)
                                .admission(admission)
                                .steering(steering)
                                .tool_search(crate::builtin::tool_search_factory())
                                .tool_rows(crate::builtin::tool_rows(
                                    session.declarer(),
                                    session.state_setter(),
                                ))
                        }),
                        budget,
                        idle,
                        warm,
                        // `fiber ask --resume` runs one turn with no client.
                        !one_turn,
                        supervised,
                        reviewer,
                        limits,
                        retry,
                        cancel,
                        slot,
                    )
                    .map(|()| None)
                },
            );
            servers.stop();
            close(session, log, &home, dir.path(), &workspace, &*clock);
            Ok(code)
        }
        fresh @ Start::New { .. }
        | fresh @ Start::Delegate { .. }
        | fresh @ Start::Rewound { .. } => {
            let (id, worktree, opened, supervised, case_inputs, parent, rewound, inherited) =
                match fresh {
                    Start::New {
                        id,
                        workspace,
                        supervised,
                        case_inputs,
                        inherited,
                    } => {
                        let worktree = workspace.worktree().cloned();
                        let opened = workspace.opened().cloned();
                        (
                            id,
                            worktree,
                            opened,
                            supervised,
                            case_inputs,
                            None,
                            None,
                            inherited,
                        )
                    }
                    Start::Delegate {
                        id,
                        parent,
                        workspace,
                        inherited,
                    } => {
                        let worktree = workspace.worktree().cloned();
                        let opened = workspace.opened().cloned();
                        (
                            id,
                            worktree,
                            opened,
                            false,
                            None,
                            Some(parent),
                            None,
                            inherited,
                        )
                    }
                    Start::Rewound {
                        id,
                        continuation,
                        workspace,
                        inherited,
                    } => {
                        let worktree = workspace.worktree().cloned();
                        let opened = workspace.opened().cloned();
                        (
                            id,
                            worktree,
                            opened,
                            false,
                            None,
                            None,
                            Some(continuation),
                            inherited,
                        )
                    }
                    Start::Resume(_) => unreachable!("a resumed start cannot assemble as new"),
                };
            let clock = case_inputs
                .as_ref()
                .map_or(Arc::clone(&process.clock), |inputs| {
                    inputs.case_run.session_clock()
                });
            log::crash::attach(&id);
            let dir = log::SessionDir::new(&parts.sessions, &id);
            // The session directory's log: the opening message's environment
            // names it.
            parts.prompt.session_log = dir.events().display().to_string();
            let Parts {
                sources,
                provider,
                observe,
                reviewer_observe,
                diag,
                model,
                prompt: prompt_inputs,
                reviewer,
                limits,
                reviewer_notes,
                budget,
                retry,
                handoff,
                idle,
                warm,
                caps,
                shell_read_only,
                sandbox,
                protection,
                home,
                project,
                workspace,
                sessions,
                repository,
                credential_files,
                extensions,
                locks,
                mut mcp,
                pending_mcp,
                switching,
                switchable,
                resolve,
                web_search,
            } = parts;
            let (job_emit, jobs) = late_emit::registry(&dir, &clock);
            shutdown::arm(signals, &reads);
            // Every session declares `delegate_spawn`: a delegate is its own
            // session, so the child re-executes this binary.
            let crate::switch::quota::QuotaReaders {
                door: quota,
                model: model_quota,
                notice,
            } = switching.quota_readers();
            let delegates = crate::delegates::Delegates::new(
                fiber_path.clone(),
                home.clone(),
                id.clone(),
                workspace.clone(),
                sessions.clone(),
                jobs.clone(),
                Arc::clone(&clock),
                resolve,
            )
            .with_quota(model_quota);
            // Before the log exists: a failure here, such as not finding the running
            // binary, leaves no session line; every server starts with the session too.
            // The `skill` tool answers from the loop's set: the reader shares it,
            // so a skill added after the opening stays unknown until the listing
            // has it (`docs/system-prompt.md`, "Added and removed skills").
            let skill_set = r#loop::SkillSet::new(prompt_inputs.clone(), &workspace);
            let skills: Arc<dyn contract::skills::Skills> = Arc::new(skill_set.reader());
            let web_search = crate::builtin::web_search(
                web_search.as_deref(),
                extensions.search_backend().as_ref(),
            )?;
            // The connection timing rides the session's MCP starts at debug; at
            // info no callable is built, so starting costs the one level check.
            let timing = if diag.debugging() {
                Some(diag.timings())
            } else {
                None
            };
            // The create line's writer opens here, before the session file
            // attaches: its `ts` is the writer's open time, its `ms` the stamped
            // creation. At `info` there is no timing, so nothing opens.
            if let (Some(timing), Some(opened)) = (&timing, &opened) {
                opened(timing);
            }
            // The registry is built above, before any tool runs: installing the
            // session's timing times a background start as `jobs.start`.
            if let Some(timing) = &timing {
                jobs.install_timing(Arc::clone(timing));
            }
            let pending_mcp = match pending_mcp {
                Some(pending) => pending,
                None => {
                    let _ = extensions.finish_processes(&support::cancel::Token::new());
                    return Err(failed(ErrorCode::IoFailed, "MCP startup was not prepared"));
                }
            };
            // The session's sink observes each `web_fetch` request at debug; at
            // any other level the fetch stays silent.
            let debug = diag
                .debugging()
                .then(|| Arc::clone(&diag) as Arc<dyn DebugLog>);
            let (tools, infos, driver, session_servers) = match mcp_servers::session_tools(
                fiber_path.clone(),
                &home,
                &workspace,
                &dir.artifacts(),
                &clock,
                &jobs,
                &locks,
                &project,
                pending_mcp,
                web_search,
                &delegates,
                skills,
                extensions.tools(),
                timing.clone(),
                debug,
                shell_read_only.clone(),
                sandbox.clone(),
                Some(protection),
            ) {
                Ok(tools) => tools,
                Err(failure) => {
                    let _ = extensions.finish_processes(&support::cancel::Token::new());
                    return Err(failure);
                }
            };
            let delegates = Arc::new(delegates);
            let forget = Arc::clone(&session_servers.forget);
            let hosted_stands = crate::switch::hosted_stands(&tools);
            let permissions = ask_permissions(
                &home,
                &project,
                workspace.to_string_lossy().into_owned(),
                credential_files,
                &clock,
            );
            let offer = Arc::new(extensions::SessionOffer::new(
                &home,
                &project,
                &workspace,
                crate::session_extensions::fetch(&clock),
            ));
            let log = match Log::create(&sessions, id.clone(), Arc::clone(&clock)) {
                Ok(log) => Arc::new(log),
                Err(e) => {
                    session_servers.servers.stop();
                    return Err(failed(e.code(), e));
                }
            };
            // `Log::create` writes no event line: installing before any append
            // times every write the process makes.
            if timing.is_some() {
                log.install_timing(Arc::clone(&diag));
            }
            // The durable log exists: the diagnostic writer takes the session's
            // file and id, so request, extension and memory lines share them.
            diag.attach(&id);
            job_emit.set(Arc::new(log::WeakEmit::new(&log)) as _);
            let event_output: Box<dyn io::Write + Send> = if case_inputs.is_some() {
                Box::new(io::sink())
            } else {
                Box::new(io::stdout())
            };
            let session = match Session::open_with_timing(
                &home,
                dir.path(),
                &log,
                Arc::clone(&clock),
                infos,
                event_output,
                timing,
            ) {
                Ok(session) => session,
                Err(e) => {
                    session_servers.servers.stop();
                    return Err(Failure::from(e));
                }
            };
            session.shell(driver);
            session.notes_workspace(workspace.clone());
            session.jobs(jobs.clone());
            session.repository_code(
                Arc::clone(&offer) as Arc<dyn contract::repository::RepositoryCode>
            );
            session.images(Arc::clone(&session_servers.images));
            session.hooks(Arc::clone(&extensions) as Arc<dyn contract::hook::Hooks>);
            session
                .extensions(Arc::clone(&extensions) as Arc<dyn contract::extension::ExtensionDoor>);
            extensions.emit_to(Arc::new(log::WeakEmit::new(&log)));
            extensions.drive_to(session.driver());
            let switching = Arc::new(switching.diagnostics(Arc::clone(&diag)));
            let host_model = Arc::new(crate::model_host::ModelHost::new(
                switching.reacher(Arc::new(crate::connect)),
                budget,
                &retry,
                dir.path().to_path_buf(),
                id.clone(),
            ));
            extensions.model_to(host_model.clone());
            extensions
                .delegate_to(Arc::clone(&delegates) as Arc<dyn contract::extension::HostDelegate>);
            // A session case with nobody attached denies where it would ask a
            // person, as a headless run does: only the person fallback is removed,
            // so `attended: false` with a reviewer still allows through it.
            let case_attended = case_inputs
                .as_ref()
                .is_none_or(|inputs| inputs.case_run.attended());
            let answerable = effective_answerable(!one_turn && case_attended, supervised);
            extensions.answerable(answerable);
            extensions.browse_with(
                Arc::new(extensions::SystemBrowser::default()),
                session.attended(),
            );
            session.skills(r#loop::skills(&prompt_inputs, &workspace));
            // The session's MCP prompt rows, tagged with each server's name:
            // the `commands` answer lists them beside skills, and `/name` runs
            // them through the fetch below (`docs/mcp.md`, "Prompts and
            // resources"). Shadowed prompts join the startup notices.
            let prompt_rows = session_servers.prompts.commands();
            let listed = r#loop::commands(
                &prompt_inputs,
                &workspace,
                &prompt_rows,
                &extensions.commands(),
            );
            mcp.notices.extend(listed.notices);
            // The turn-start check measures the runnable set against these rows,
            // so a skill added before the opening lists still refreshes them.
            skill_set.sync_commands(&listed.runnable);
            session.commands(listed.rows);
            session.quota(quota);
            let door = crate::switch::Door {
                declare: session.declarer(),
                hosted_stands,
                backend: extensions.search_backend(),
            };
            let current = Arc::new(crate::session_extensions::Current::new(extensions));
            let (commands_refresh, commands_sync) =
                command_update_wiring(&current, &session, &skill_set);
            let readers = switching
                .credential_readers(switching.clone().closure(door), model.reference.clone());
            session.credentials(readers.labels, readers.admitted);
            let switcher = readers.prepare;
            let (reload, load, servers, prompts, server_failures) = crate::reload::State::new(
                sources,
                prompt_inputs.clone(),
                current.clone(),
                switching.clone(),
                &tools,
                &session,
                host_model,
                Arc::clone(&delegates) as Arc<dyn contract::extension::HostDelegate>,
                session_servers,
                answerable,
                log.clone(),
                diag.clone(),
                shell_read_only,
                sandbox,
                std::sync::Arc::new(std::sync::Mutex::new(repository)),
                commands_sync,
            );
            // Every notice the startup already wrote, including the ones
            // `mcp_servers_started` writes after this read: the late preparer
            // drops the ones it would repeat and keeps the new ones.
            let written = crate::session_extensions::startup_written(&current.get(), &mcp.notices);
            let providers = reload.mcp_providers(&server_failures);
            let (reloader, late) = reload.preparers(written);
            session.mcp_servers(providers.refresh);
            session.mcp_login(providers.login);
            session.personal_source_check(crate::prompt_files::personal_source_check(
                prompt_inputs.home.clone(),
                prompt_inputs.agents_home.clone(),
                Arc::clone(&prompt_inputs.clock),
            ));
            let cancel = Arc::new(r#loop::TurnCancel::default());
            // A signal while armed: nothing was written to the session log, so nothing more is.
            let reads = switching.reads();
            if let Some(code) = shutdown::start(signals, &cancel, &session, jobs.clone(), reads) {
                servers.stop();
                close(session, log, &home, dir.path(), &workspace, &*clock);
                return Ok(code);
            }
            let inbox_wake = session.inbox_wake();
            let rewrites = session.rewrites();
            let admission = session.admission();
            let steering = session.steering();
            // The fetch runs a prompt row through its server, starting a lazy
            // one as a first tool call does (`docs/mcp.md`, "Prompts and
            // resources"). No logic lives here beyond that call.
            let fetch_prompts = prompts.clone();
            let fetch: r#loop::FetchPrompt = Arc::new(move |server, prompt, text, cancel| {
                fetch_prompts.get(server, prompt, text, cancel)
            });
            let case_driver = case_inputs.as_ref().and_then(|inputs| {
                let case = &inputs.case_run;
                let mut watcher = log.watch_all();
                let events = Box::new(move |wait| {
                    watcher
                        .recv_timeout(wait)
                        .map(|result| result.map_err(|error| error.to_string()))
                });
                let case_cancel: Arc<dyn Fn() + Send + Sync> = {
                    let cancel = Arc::clone(&cancel);
                    Arc::new(move || {
                        cancel.cancel();
                    })
                };
                match case.start(session.driver(), events, case_cancel) {
                    Ok(driver) => Some(driver),
                    Err(error) => {
                        case.record_failure(error.to_string());
                        None
                    }
                }
            });
            let run_one_turn = one_turn || (case_inputs.is_some() && case_driver.is_none());
            let code = run_turn(
                &session,
                &log,
                dir.path(),
                prompt,
                run_one_turn,
                cancel,
                &diag,
                &current,
                |inbox, cancel, slot| {
                    // The commit writes the session's first lines together, so a
                    // signal recorded before them writes nothing: `Loop::start`
                    // writes `session_started`, which `fiber_started` follows
                    // (`docs/events.md`); a rewind's start writes the
                    // `session_started` that continues the old log instead, and a
                    // delegate opens through `Loop::delegate`, naming its parent.
                    let committed = signals.commit(|| {
                        let started = match (rewound, parent) {
                            (Some(start), _) => {
                                let is_fork =
                                    matches!(&start.how, r#loop::Continuation::Fork { .. });
                                let session = r#loop::Session {
                                    log: Arc::clone(&log),
                                    provider,
                                    model,
                                    prompt: prompt_inputs,
                                    inbox,
                                    tools: r#loop::capped(tools, &caps),
                                    permissions,
                                };
                                if is_fork {
                                    Loop::fork_with_added(session, start, worktree, inherited)
                                } else {
                                    Loop::rewound_with_worktree(session, start, worktree)
                                }
                            }
                            // A fresh spawn records its inherited roots here,
                            // after `session_started` and before the opening. A
                            // fork's roots are seeded in its point-continuation arm.
                            (None, Some(parent)) => Loop::delegate_with_added(
                                r#loop::Session {
                                    log: Arc::clone(&log),
                                    provider,
                                    model,
                                    prompt: prompt_inputs,
                                    inbox,
                                    tools: r#loop::capped(tools, &caps),
                                    permissions,
                                },
                                parent,
                                worktree,
                                inherited,
                            ),
                            (None, None) if supervised => Loop::supervised(
                                r#loop::Session {
                                    log: Arc::clone(&log),
                                    provider,
                                    model,
                                    prompt: prompt_inputs,
                                    inbox,
                                    tools: r#loop::capped(tools, &caps),
                                    permissions,
                                },
                                worktree,
                            ),
                            (None, None) => Loop::start(
                                r#loop::Session {
                                    log: Arc::clone(&log),
                                    provider,
                                    model,
                                    prompt: prompt_inputs,
                                    inbox,
                                    tools: r#loop::capped(tools, &caps),
                                    permissions,
                                },
                                worktree,
                            ),
                        };
                        started.and_then(|looped| {
                            r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), false)?;
                            session_extensions::written(&log, &current.get())?;
                            // The providers load reports the same shadow the
                            // session's load reported: `written` wrote that half.
                            let notices =
                                crate::session_extensions::unwritten(mcp.notices, &current.get());
                            r#loop::mcp_servers_started(&log, server_failures, notices)?;
                            Ok(looped)
                        })
                    });
                    let started = match committed {
                        Err(code) => return Ok(Some(code)),
                        Ok(started) => started,
                    };
                    finish(
                        started.map(|looped| {
                            let looped = session_extensions::hooked(
                                looped.jobs(jobs).quota(notice),
                                &current,
                            );
                            let looped = session_extensions::released(looped, Arc::clone(&current));
                            looped
                                .diagnostics(Arc::clone(&diag))
                                .observed(observe)
                                .reviewer_observed(reviewer_observe)
                                .reviewer_notes(reviewer_notes)
                                .handoff(handoff)
                                .on_handoff(forget)
                                .switcher(switcher, switchable)
                                .reloader(reloader)
                                .reconnector(providers.reconnect)
                                .late_extensions(late)
                                .repository_code(offer)
                                .approved_code(load)
                                .skill_set(skill_set)
                                .commands_refresh(commands_refresh)
                                .server_prompts(r#loop::ServerPrompts {
                                    rows: prompt_rows,
                                    fetch,
                                })
                                .inbox_wake(inbox_wake)
                                .on_rewrite(rewrites)
                                .admission(admission)
                                .steering(steering)
                                .tool_search(crate::builtin::tool_search_factory())
                                .tool_rows(crate::builtin::tool_rows(
                                    session.declarer(),
                                    session.state_setter(),
                                ))
                        }),
                        budget,
                        idle,
                        warm,
                        // One-turn `fiber ask` and unattended session cases run with no client that may answer (`docs/permissions.md`, "Headless").
                        !run_one_turn && case_attended,
                        supervised,
                        reviewer,
                        limits,
                        retry,
                        cancel,
                        slot,
                    )
                    .map(|()| None)
                },
            );
            if let Some(case_driver) = case_driver {
                match case_driver.join() {
                    Ok(()) => {}
                    Err(_) => {
                        if let Some(inputs) = &case_inputs {
                            inputs
                                .case_run
                                .record_failure("the case driver panicked".to_owned());
                        }
                    }
                }
            }
            servers.stop();
            close(session, log, &home, dir.path(), &workspace, &*clock);
            Ok(code)
        }
    }
}

/// As [`run_new`], opening the worktree's create line once the diagnostic
/// writer exists: `opened` is the isolation's opener, held above while
/// `run_new` runs and called right after the session timing is built,
/// before the writer attaches. `None` skips it, so a run without a fresh
/// worktree writes no create line.
#[allow(
    clippy::too_many_arguments,
    reason = "session execution carries its isolation, case and startup inputs"
)]
pub(crate) fn run_new_with(
    id: SessionId,
    overrides: Vec<String>,
    prompt: Option<String>,
    one_turn: bool,
    worktree: Option<Worktree>,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    process: &crate::startup::Process,
    case_inputs: Option<CaseInputs>,
    parent: Option<Parent>,
    rewound: Option<r#loop::Rewound>,
    reads: Arc<crate::switch::Reads>,
    opened: Option<doors::WorktreeOpener>,
    supervised: bool,
    inherited: Vec<PathBuf>,
) -> Result<i32, Failure> {
    let _hold = OpenedHold::set(opened);
    run_new(
        id,
        overrides,
        prompt,
        one_turn,
        worktree,
        clock,
        signals,
        process,
        case_inputs,
        parent,
        rewound,
        reads,
        supervised,
        inherited,
    )
}

/// Prints a startup failure, asking about a recorded signal first: a
/// signal that arrived before `fiber_started` exits with its code and
/// writes nothing to the session log (`docs/invocation.md`, "Shutdown").
pub(crate) fn report(signals: &doors::Signals, failure: Failure) -> i32 {
    signals.recorded().unwrap_or_else(|| ask_failed(failure))
}

#[cfg(test)]
#[path = "session_command_tests.rs"]
mod tests;

/// Ends a session process, however it ran: closes the door side, then
/// appends the session's `recent.jsonl` row with what it stopped on, its
/// last `session_status` (`docs/state.md`, "Recently exited sessions").
/// A session `close` deleted, never prompted, leaves nothing behind.
pub(crate) fn close(
    session: Session,
    log: Arc<Log>,
    home: &Path,
    dir: &Path,
    workspace: &Path,
    clock: &dyn contract::clock::Clock,
) {
    let status = log
        .latest("session_status")
        .and_then(|line| serde_json::from_value(line.payload.into()).ok());
    session.close(log);
    // A row that cannot be written loses only the listing, not the log.
    hub::append_exited(home, dir, workspace, status, clock).unwrap_or(());
}

#[cfg(test)]
mod opened_hold_tests {
    #![allow(clippy::panic, reason = "test assertions")]

    use super::*;

    /// The hold hands the slot back on drop: the inner drop restores the
    /// outer opener, and the outer drop clears the slot, so a later
    /// `run_new_with` on the same thread sees no earlier opener.
    #[test]
    fn drop_restores_the_previous_opener_then_clears() {
        let first: doors::WorktreeOpener = Arc::new(|_: &contract::diag::Timing| {});
        let second: doors::WorktreeOpener = Arc::new(|_: &contract::diag::Timing| {});
        // Whatever the thread ran before: start known, hand back after.
        let stash = OPENED.with(|cell| cell.take());
        {
            let _outer = OpenedHold::set(Some(Arc::clone(&first)));
            {
                let _inner = OpenedHold::set(Some(Arc::clone(&second)));
                assert!(
                    OPENED.with(|cell| cell
                        .borrow()
                        .as_ref()
                        .is_some_and(|held| Arc::ptr_eq(held, &second))),
                    "the inner hold sets the slot"
                );
            }
            assert!(
                OPENED.with(|cell| cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|held| Arc::ptr_eq(held, &first))),
                "the inner drop hands the slot back to the outer opener"
            );
        }
        assert!(
            OPENED.with(|cell| cell.borrow().is_none()),
            "the outer drop clears the slot"
        );
        OPENED.with(|cell| {
            cell.replace(stash);
        });
    }
}
