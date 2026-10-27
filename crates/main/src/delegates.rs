//! Wiring a Fiber delegate into a session (`docs/delegates.md`, "Choosing a
//! model", "Lifetime" and "Talking to a delegate"): resolving its `fiber:`
//! reference through the installed providers, building the child session's
//! command, watching the child's socket as an ordinary client, and steering
//! or resuming it through `delegate_message`. `main` builds one [`Delegates`]
//! per session and hands it to the tool seam, which registers
//! `delegate_spawn` and `delegate_message` from it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use contract::clock::{Clock, utc_time};
use contract::events::VariablesSource;
use contract::shapes::{Failure, Point};
use contract::{ActionId, ErrorCode, Seq, SessionId};
use extensions::{Quota, QuotaEntry, Window, WindowUse};

use crate::switch::quota::{ModelQuotaRead, QuotaView};

static DEPTH: OnceLock<u32> = OnceLock::new();

pub(crate) fn set_depth(depth: u32) {
    let _first = DEPTH.set(depth);
}

pub(crate) fn depth() -> u32 {
    DEPTH.get().copied().unwrap_or(0)
}

/// How large a delegate's `events.jsonl` may grow before the runner stops
/// it as `failed` `output_cap`: the 5 GB every job's output file gets
/// (`docs/tools.md`, "Background jobs").
pub(crate) const OUTPUT_CAP: u64 = 5_000_000_000;

/// Everything one session needs to offer `delegate_spawn` and
/// `delegate_message`: who the child belongs to and where it logs, and the
/// closures the runner drives. Built per session, because the parent
/// session id differs for a new and a resumed session.
pub(crate) struct Delegates {
    depth: u32,
    home: PathBuf,
    /// The parent session: the child's `--parent`.
    parent: SessionId,
    /// The parent's workspace, which the delegate shares.
    workspace: PathBuf,
    /// Where delegates log: each child's `events.jsonl` lives under
    /// `<sessions>/<session id>`.
    sessions: PathBuf,
    /// The session's jobs.
    jobs: Arc<jobs::Registry>,
    /// The session clock.
    clock: Arc<dyn Clock>,
    /// Resolves a `delegate_spawn` `model` argument, and lists the
    /// catalogue every refusal carries.
    resolve: jobs::Resolve,
    /// Builds the child's command.
    launch: jobs::Launch,
    /// Watches the child's socket.
    watch: jobs::Watch,
    /// Steers the child's session over its socket.
    steer: jobs::Steer,
    /// Reads the child's log boundary for a resume.
    last_seq: jobs::LastSeq,
    /// Reads the child's durable worktree in its own project.
    worktree_record: jobs::WorktreeRecord,
    /// Renders the quota section of `delegate_models` on every call.
    quota: Option<ModelQuotaRead>,
    /// The last list `retain` was given: a start whose extension it lacks
    /// began after a removal reload, so it ends stopped. `None` until
    /// the first `retain`.
    loaded: Mutex<Option<Vec<String>>>,
}

impl Delegates {
    /// This session's id, which its delegates receive as their parent.
    pub(crate) fn session(&self) -> &SessionId {
        &self.parent
    }

    /// Wires one session's delegates: `fiber` is the running binary the
    /// child re-executes, and `home` is Fiber home, where the child's
    /// socket lives.
    #[allow(
        clippy::too_many_arguments,
        reason = "one session's delegate wiring: its ids, paths, jobs, clock and resolver"
    )]
    pub(crate) fn new(
        fiber: PathBuf,
        home: PathBuf,
        parent: SessionId,
        workspace: PathBuf,
        sessions: PathBuf,
        jobs: Arc<jobs::Registry>,
        clock: Arc<dyn Clock>,
        resolve: jobs::Resolve,
    ) -> Self {
        let launch = launcher(&fiber);
        let watch = watcher(&home);
        let steer = steerer(&home);
        let last_seq = last_seq_reader(&home);
        let worktree_record = worktree_record_reader(&home);
        Self {
            depth: depth(),
            home,
            parent,
            workspace,
            sessions,
            jobs,
            clock,
            resolve,
            launch,
            watch,
            steer,
            last_seq,
            worktree_record,
            quota: None,
            loaded: Mutex::new(None),
        }
    }

    /// Wires the model-facing quota reader: `models()` renders its snapshot
    /// on every call. Without it the quota section reads `Quota: none`.
    pub(crate) fn with_quota(self, quota: ModelQuotaRead) -> Self {
        Self {
            quota: Some(quota),
            ..self
        }
    }

    /// The `delegate_fork` tool this session declares: the same wiring
    /// `tool()` hands `delegate_spawn`, minus the model resolver a fork
    /// never uses, plus the point reader below. A fork carries the
    /// calling message's model, so nothing resolves here.
    pub(crate) fn fork(&self) -> jobs::DelegateFork {
        jobs::DelegateFork {
            wiring: self.wiring(),
            point: fork_point_reader(&self.sessions, &self.parent),
        }
    }

    /// The `delegate_spawn` tool this session declares.
    pub(crate) fn tool(&self) -> jobs::DelegateSpawn {
        jobs::DelegateSpawn {
            wiring: self.wiring(),
            resolve: self.resolve.clone(),
        }
    }

    fn wiring(&self) -> jobs::DelegateWiring {
        let home = self.home.clone();
        jobs::DelegateWiring {
            create: worktree_creator(&self.home),
            finish: worktree_finisher(&self.home, &self.clock),
            depth: self.depth,
            registry: Arc::clone(&self.jobs),
            parent: self.parent.clone(),
            workspace: self.workspace.clone(),
            inherited: inherited_roots_reader(&self.sessions, &self.parent),
            events_for: Arc::new(move |workspace, id| {
                let sessions = log::sessions_dir(&home, &doors::project(workspace));
                log::SessionDir::new(&sessions, id).events()
            }),
            clock: Arc::clone(&self.clock),
            // A stop waits for the delegate's own shutdown before SIGKILL,
            // as the parent's shutdown bound does (`docs/invocation.md`,
            // "Shutdown").
            bound: doors::SHUTDOWN_BOUND,
            cap: OUTPUT_CAP,
            launch: Arc::clone(&self.launch),
            watch: Arc::clone(&self.watch),
        }
    }
}

impl contract::extension::HostDelegate for Delegates {
    /// The session's delegate start for `host.delegate`: the same
    /// `start_extension` the model's `delegate_spawn` runs, so both paths
    /// share one registry, one depth and one start gate. A start whose
    /// extension the last retained list lacks began after a removal
    /// reload, so it ends stopped, while the caller still gets its id.
    /// `retain` stores its list before it stops anything, and `cancel`
    /// counts the stops that ended a job.
    ///
    /// `loaded` is held alone: `retain` releases it before
    /// `retain_extensions`, and `start` reads it after `start_extension`
    /// returns, so no code takes the registry's start gate while holding
    /// it (`docs/architecture.md`, "Lock order").
    fn start(
        &self,
        extension: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        bind: &mut dyn FnMut(&contract::JobId),
    ) -> Result<contract::JobId, contract::shapes::Failure> {
        let id = self
            .tool()
            .start_extension(extension, arguments, bind)
            .map_err(|error| contract::shapes::Failure::from(&error))?;
        let listed = support::lock(&self.loaded)
            .as_ref()
            .is_none_or(|loaded| loaded.iter().any(|name| name == extension));
        if !listed {
            contract::jobs::Jobs::stop(self.jobs.as_ref(), &id);
        }
        Ok(id)
    }

    fn retain(&self, loaded: &[String]) -> usize {
        *support::lock(&self.loaded) = Some(loaded.to_vec());
        self.jobs.retain_extensions(loaded)
    }

    fn cancel(&self, jobs: &[contract::JobId]) -> usize {
        jobs.iter()
            .filter(|id| contract::jobs::Jobs::stop(self.jobs.as_ref(), id))
            .count()
    }
}

impl Delegates {
    /// The `delegate_message` tool this session declares: steers a running
    /// delegate at its next step boundary, or resumes a finished one. It
    /// shares the registry, launch, watch and depth `tool()` hands
    /// `delegate_spawn`, so a message finds this session's own delegates
    /// and no other session's.
    pub(crate) fn message(&self) -> jobs::DelegateMessage {
        jobs::DelegateMessage {
            finish: worktree_finisher(&self.home, &self.clock),
            depth: self.depth,
            registry: Arc::clone(&self.jobs),
            parent: self.parent.clone(),
            clock: Arc::clone(&self.clock),
            // The same shutdown bound and output cap as `tool()`: one
            // stop and one closing wait for both tools.
            bound: doors::SHUTDOWN_BOUND,
            cap: OUTPUT_CAP,
            launch: Arc::clone(&self.launch),
            watch: Arc::clone(&self.watch),
            steer: Arc::clone(&self.steer),
            last_seq: Arc::clone(&self.last_seq),
            worktree_record: Arc::clone(&self.worktree_record),
        }
    }

    /// The `delegate_models` tool this session declares: the catalogue
    /// built with the session's resolver, with the quota section appended.
    pub(crate) fn models(&self) -> jobs::DelegateModels {
        let quota = self.quota.clone();
        jobs::DelegateModels {
            catalogue: Arc::clone(&self.resolve.catalogue),
            quota: Arc::new(move || match quota {
                Some(ref reader) => quota_text(&reader()),
                None => String::from("Quota: none"),
            }),
        }
    }
}

/// Resolves a delegate's model through the installed providers and the
/// configured roles: an argument that equals a role name is that role,
/// anything else is a full `fiber:<provider>/<model>[:<level>]` reference
/// (`docs/delegates.md`, "Choosing a model"). An explicit level must be
/// one the model takes, checked with the same rule `--model` uses. A
/// role's target is resolved as a reference only: roles never chain. Both
/// the lookup and the catalogue are built from one providers and
/// configuration snapshot, so they never disagree within a session.
pub(crate) fn resolver(
    providers: &extensions::Providers,
    config: &config::Config,
) -> jobs::Resolve {
    let providers = providers.clone();
    let config = config.clone();
    let listed = providers.clone();
    let read = config.clone();
    let credential_providers = providers.clone();
    let credential_config = config.clone();
    jobs::Resolve {
        lookup: Arc::new(move |argument| lookup(&providers, &config, argument)),
        catalogue: Arc::from(catalogue(&listed, &read)),
        credential: Arc::new(move |argument| {
            credential_for(&credential_providers, &credential_config, argument)
        }),
    }
}

/// Resolves one `delegate_spawn` `model` argument to the
/// `provider/model[:level]` the child is started with, or refuses it with
/// one reason sentence.
fn lookup(
    providers: &extensions::Providers,
    config: &config::Config,
    argument: &str,
) -> Result<String, String> {
    if let Some(target) = role_target(config, argument) {
        return resolve_reference(providers, config, &target)
            .map_err(|reason| format!("Role {argument:?} maps to {target:?}: {reason}"));
    }
    resolve_reference(providers, config, argument)
}

/// A role's credential label as configured now: the object's string
/// `credential`, whatever its `model` is. `None` for a string role, an
/// object without one, a non-string `credential`, or a non-role argument.
/// A repository's role names no credential: its winning source is the
/// repository layer, and its `credential` is ignored, so it reads as
/// `None` too (`docs/configuration.md`, `roles."<name>"`).
fn role_credential(config: &config::Config, name: &str) -> Option<String> {
    let key = format!("roles.{}", config::path::display(&[name.to_owned()]));
    let (value, source) = config.get(&key, None)?;
    if matches!(source, config::Source::Repository(_)) {
        return None;
    }
    value.get("credential")?.as_str().map(str::to_owned)
}

/// The credential label the `model` argument's delegate runs under, for
/// the `credential` closure: the role's label when it names one the
/// provider has, `None` for a full reference or a role without one. A
/// label that names no credential fails with `credential_missing`, naming
/// the labels the provider has; a scripted provider takes no label, so
/// any label asked for one fails the same way. The model sees only role
/// names, never labels: the label never reaches the catalogue.
fn credential_for(
    providers: &extensions::Providers,
    config: &config::Config,
    argument: &str,
) -> Result<Option<String>, String> {
    let Some(label) = role_credential(config, argument) else {
        return Ok(None);
    };
    let Some(target) = role_target(config, argument) else {
        return Ok(None);
    };
    let rest = target.strip_prefix("fiber:").unwrap_or(&target);
    let Ok(resolved) = providers.resolve(rest) else {
        return Ok(None);
    };
    if let Err(failure) = crate::scripted::label(resolved.provider, Some(&label)) {
        return Err(failure.message);
    }
    let labels = config.credentials().labels(resolved.provider);
    if labels.contains(&label) {
        Ok(Some(label))
    } else {
        Err(crate::credential::no_label(&resolved.provider.name, &label, &labels).message)
    }
}

/// The quota section `delegate_models` appends to the catalogue: one line
/// per provider credential label, sorted by provider then credential
/// (`docs/tools.md`, "What the model sees"). A loading view renders the
/// single `Quota: loading` line before any per-label entry; a loaded view
/// with no credentialed label renders `Quota: none`.
fn quota_text(view: &QuotaView) -> String {
    let labels = match view {
        QuotaView::Loading => return String::from("Quota: loading"),
        QuotaView::Loaded(labels) if labels.is_empty() => return String::from("Quota: none"),
        QuotaView::Loaded(labels) => labels,
    };
    let mut rows = labels.clone();
    rows.sort_by(|a, b| (&a.provider, &a.credential).cmp(&(&b.provider, &b.credential)));
    let mut text = String::from("Quota:");
    for row in &rows {
        text.push_str(&format!(
            "\n{}/{}: {}",
            row.provider,
            row.credential,
            quota_entry(&row.entry, row.pending)
        ));
    }
    text
}

/// What the model sees for one credential label: its value, `no quota
/// reported` for a provider without `quota()`, `unreachable` with the
/// last value and its age where there is one, or `no value yet` while
/// its first fetch is still in flight.
fn quota_entry(entry: &QuotaEntry, pending: bool) -> String {
    match entry {
        QuotaEntry::Reported(quota) => quota_value(quota),
        QuotaEntry::NoQuota => String::from("no quota reported"),
        QuotaEntry::Unreachable {
            last: Some((quota, age)),
        } => format!(
            "unreachable (last: {}, {} ago)",
            quota_value(quota),
            quota_age(*age)
        ),
        QuotaEntry::Unreachable { last: None } if pending => String::from("no value yet"),
        QuotaEntry::Unreachable { last: None } => String::from("unreachable"),
    }
}

/// A fetched quota value: its windows joined with `, `, or its credit
/// remaining with the limit where the vendor reports one. An empty window
/// list reads as a provider with nothing to report.
fn quota_value(quota: &Quota) -> String {
    match quota {
        Quota::Windows(windows) if windows.is_empty() => String::from("no quota reported"),
        Quota::Windows(windows) => windows
            .iter()
            .map(quota_window)
            .collect::<Vec<_>>()
            .join(", "),
        Quota::Credit { remaining, limit } => match limit {
            Some(limit) => format!("credit {remaining} remaining of {limit}"),
            None => format!("credit {remaining} remaining"),
        },
    }
}

/// One usage window: its name and percent used with the reset time where
/// the provider reports one, or as reset once the reset time has passed.
fn quota_window(window: &Window) -> String {
    match &window.used {
        WindowUse::Percent { used, resets_at } => {
            let percent = used.round();
            match resets_at {
                Some(at) => format!(
                    "{} {percent}% used (resets {})",
                    window.name,
                    reset_time(*at)
                ),
                None => format!("{} {percent}% used", window.name),
            }
        }
        WindowUse::Reset => format!("{} reset", window.name),
    }
}

/// A reset time in UTC: reset times are built from the epoch plus a
/// duration when quota is parsed, so a failure reads as the epoch.
fn reset_time(at: std::time::SystemTime) -> String {
    utc_time(
        at.duration_since(UNIX_EPOCH)
            .map(|past| past.as_secs())
            .unwrap_or(0),
    )
}

/// An unreachable age in the largest whole unit that fits.
fn quota_age(age: std::time::Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// A role's target reference as configured now: a string value is the
/// target, and an object's string `model` is the target while its
/// `credential` is ignored. Any other shape was already rejected when
/// configuration loaded (`crates/config/src/keys.rs`, `Kind::Role`), so it
/// resolves as no role.
fn role_target(config: &config::Config, name: &str) -> Option<String> {
    let merged = config.merged(None);
    let roles = merged.get("roles")?;
    let value = roles.get(name)?;
    if let Some(target) = value.as_str() {
        return Some(target.to_owned());
    }
    value.get("model")?.as_str().map(str::to_owned)
}

/// Resolves a full `fiber:<provider>/<model>[:<level>]` reference to the
/// `provider/model[:level]` the child is started with, or refuses it with
/// one reason sentence: `Unknown delegate model "<arg>".` for anything
/// not naming an installed model, or the thinking rule's message for a
/// level the model does not take. Without the prefix the reference is
/// refused even when the plain lookup would take it, and a bare id
/// without a provider is refused too.
fn resolve_reference(
    providers: &extensions::Providers,
    config: &config::Config,
    reference: &str,
) -> Result<String, String> {
    let unknown = || format!("Unknown delegate model {reference:?}.");
    let Some(resolved) = providers.delegate_model(reference) else {
        return Err(unknown());
    };
    if let Some(level) = resolved.thinking {
        let reference = resolved.reference();
        let mut notices = Vec::new();
        if let Err(failure) = crate::settings::thinking(
            Some(level),
            None,
            config,
            resolved.model,
            &reference,
            &mut notices,
        ) {
            return Err(failure.message);
        }
        return Ok(format!("{reference}:{level}"));
    }
    Ok(resolved.reference())
}

/// The catalogue text `delegate_models` returns and every model refusal
/// lists: the configured roles, what each maps to now, and the full
/// references available. Roles and references are each sorted by name. A
/// role whose target is not a usable reference is marked ` (unavailable)`.
fn catalogue(providers: &extensions::Providers, config: &config::Config) -> String {
    let mut roles: Vec<(String, String)> = config
        .merged(None)
        .get("roles")
        .and_then(|roles| roles.as_object())
        .map(|roles| {
            roles
                .iter()
                .filter_map(|(name, value)| {
                    let target = if let Some(target) = value.as_str() {
                        target.to_owned()
                    } else {
                        value.get("model")?.as_str()?.to_owned()
                    };
                    Some((name.clone(), target))
                })
                .collect()
        })
        .unwrap_or_default();
    roles.sort();
    let mut references: Vec<String> = Vec::new();
    for name in providers.names() {
        if let Some(provider) = providers.get(name) {
            for model in &provider.models {
                let reference = format!("fiber:{name}/{}", model.id);
                if model.thinking_levels.is_empty() {
                    references.push(reference);
                } else {
                    let levels = model
                        .thinking_levels
                        .iter()
                        .map(|level| level.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    references.push(format!("{reference} (thinking: {levels})"));
                }
            }
        }
    }
    references.sort();
    let roles_text = if roles.is_empty() {
        String::from("Roles: none")
    } else {
        let mut text = String::from("Roles:");
        for (name, target) in &roles {
            let marker = if resolve_reference(providers, config, target).is_ok() {
                ""
            } else {
                " (unavailable)"
            };
            text.push_str(&format!("\n{name}: {target}{marker}"));
        }
        text
    };
    let references_text = if references.is_empty() {
        String::from("References: none")
    } else {
        let mut text = String::from("References:");
        for reference in &references {
            text.push_str(&format!("\n{reference}"));
        }
        text
    };
    format!("{roles_text}\n{references_text}")
}

/// Builds the child's command from its launch: the running binary
/// re-executed as a one-turn child session of its parent
/// (`docs/delegates.md`, "Lifetime"). The runner sets the stdio, the
/// process group and the lifeline around it.
pub(crate) fn launcher(fiber: &Path) -> jobs::Launch {
    let fiber = fiber.to_path_buf();
    Arc::new(move |launched: &jobs::Launched| command(&fiber, launched))
}

fn worktree_creator(home: &Path) -> jobs::CreateWorktree {
    let home = home.to_owned();
    Arc::new(
        #[allow(
            clippy::result_large_err,
            reason = "the jobs boundary returns its typed error by value"
        )]
        move |workspace, session| {
            let (created, shared) =
                doors::create_delegate_worktree(workspace, &home, session, &Command::output)
                    .map_err(|error| {
                        let code = match &error {
                            doors::DelegateCreateError::NotARepository { .. } => {
                                ErrorCode::InvalidArguments
                            }
                            doors::DelegateCreateError::Failed { .. } => ErrorCode::IoFailed,
                        };
                        jobs::JobsError::DelegateWorktree {
                            failure: Failure {
                                code,
                                message: error.to_string(),
                                retry_after_ms: None,
                                provider: None,
                            },
                        }
                    })?;
            Ok((
                jobs::EndWorktree {
                    path: created.path,
                    branch: created.branch,
                    base: Some(created.base),
                    remove: true,
                },
                jobs::WorktreeGuard::new(move || drop(shared)),
            ))
        },
    )
}

fn worktree_finisher(home: &Path, clock: &Arc<dyn Clock>) -> jobs::FinishWorktree {
    let home = home.to_owned();
    let clock = Arc::clone(clock);
    Arc::new(move |end| {
        let session = SessionId(
            end.branch
                .strip_prefix("fiber/")
                .unwrap_or_default()
                .to_owned(),
        );
        doors::end_delegate_worktree(
            &home,
            &session,
            Arc::clone(&clock),
            &end.path,
            &end.branch,
            end.base.as_deref(),
            end.remove,
        )
    })
}

fn command(fiber: &Path, launched: &jobs::Launched) -> Command {
    // What the child runs as: a spawn carries its resolved model and an
    // optional credential override, a fork carries its point and neither
    // a model nor a credential, and a resume carries neither, continuing
    // the recorded session.
    let (model, overrides, resume, forked_from, forked_seq) = match &launched.start {
        jobs::Start::Fresh(jobs::Fresh::Spawn { model, credential }) => {
            // A role naming a credential label runs the child under it: the
            // label travels as a run override, so the child's `preamble_built`
            // records it. The value is a JSON string, so a label that parses
            // as JSON still arrives as a string.
            let overrides = match credential.as_deref() {
                Some(label) => {
                    let provider = model.split('/').next().unwrap_or_default();
                    let key = config::path::display(&[
                        "providers".to_owned(),
                        provider.to_owned(),
                        "credential".to_owned(),
                    ]);
                    let value = serde_json::Value::String(label.to_owned());
                    vec![format!("{key}={value}")]
                }
                None => Vec::new(),
            };
            (Some(model.clone()), overrides, false, None, None)
        }
        jobs::Start::Fresh(jobs::Fresh::Fork { from }) => (
            None,
            Vec::new(),
            false,
            Some(from.point.session_id.clone()),
            Some(from.point.seq.0),
        ),
        jobs::Start::Resume => (None, Vec::new(), true, None, None),
    };
    // A delegate carries its parent's environment, so it carries the
    // parent's source too (`docs/invocation.md`, "A session's
    // environment").
    let args = contract::session_argv::SessionArgv {
        id: launched.session_id.clone(),
        workspace: launched.workspace.clone(),
        model,
        overrides,
        prompt: Some(launched.prompt.clone()),
        resume,
        credential: None,
        worktree: launched.worktree.is_some(),
        rewound_from: None,
        continues: None,
        forked_from,
        forked_seq,
        parent: Some(launched.parent.clone()),
        delegate_id: Some(launched.job_id.clone()),
        depth: Some(launched.depth),
        added: launched.added.clone(),
        login_shell: r#loop::variables_source() == VariablesSource::LoginShell,
        // Delegates are never supervised: they answer to their parent
        // session, not to a program outside Fiber.
        supervised: false,
    };
    let mut command = Command::new(fiber);
    command.args(args.to_argv());
    command
}

/// Watches the child's socket as an ordinary client, folding its log from
/// the subscribe past the run's boundary: the jobs-local outcome over
/// `doors::watch_after` (`docs/delegates.md`, "Streams").
pub(crate) fn watcher(home: &Path) -> jobs::Watch {
    let home = home.to_path_buf();
    Arc::new(move |id, after, on_line| {
        doors::watch_after(&home, id, after, on_line).map(|watched| match watched {
            doors::Watched::Exited => jobs::Watched::Exited,
            doors::Watched::Closed { .. } => jobs::Watched::Closed,
        })
    })
}

/// Steers the child's session over its socket as an ordinary client:
/// one connection carrying the subscribe, the steer and the probe, with
/// the outcome mapped back variant by variant.
fn steerer(home: &Path) -> jobs::Steer {
    let home = home.to_path_buf();
    Arc::new(
        move |id, command, text, commit: &mut dyn FnMut() -> bool, on_queued: &mut dyn FnMut()| {
            match doors::steer(&home, id, command, text, commit, on_queued) {
                doors::Steered::Accepted => jobs::Steered::Accepted,
                doors::Steered::Rejected(failure) => jobs::Steered::Rejected(failure),
                doors::Steered::Gone => jobs::Steered::Gone,
                doors::Steered::Withheld => jobs::Steered::Withheld,
            }
        },
    )
}

/// Reads the parent's added folders at launch from its full history
/// fold. A delegate fork continues the parent's prompt at a point but
/// receives the roots current when the child launches; a person fork
/// remains point-based (`docs/events.md`, "`directory_added`"). Bound
/// at wiring to the parent's sessions and id, and read when the child
/// launches, so an addition between wiring and launch is captured. A
/// fold that cannot be read yields no roots: fewer roots only ever deny,
/// so an unreadable parent log cannot widen the child.
pub(crate) fn inherited_roots_reader(sessions: &Path, parent: &SessionId) -> jobs::InheritedRoots {
    let sessions = sessions.to_owned();
    let parent = parent.clone();
    Arc::new(move || {
        let dir = log::SessionDir::new(&sessions, &parent);
        r#loop::resumed(dir.path())
            .map(|folded| folded.added().to_vec())
            .unwrap_or_default()
    })
}

/// Reads the point a fork continues from: the parent's own log holds the
/// calling message, the call and the chain they ran inside
/// (`docs/delegates.md`, "Forks"). `S` is the latest
/// `assistant_message_started` before the calling action's
/// `tool_call_requested`, or the latest in the log when no call runs;
/// the point is `S - 1`, with the calling message's
/// `usage_recorded.model`. The `outer_action_id` chain walks to the root
/// call: a root anchored at a command (`ran_by.command_id`) is refused
/// with `invalid_arguments`, while a link the log does not hold keeps the
/// point and the message's model instead of failing the parent. A failure
/// that reads only the parent's own log is a `tool_error`.
pub(crate) fn fork_point_reader(sessions: &Path, parent: &SessionId) -> jobs::ForkPoint {
    let dir = log::SessionDir::new(sessions, parent);
    let parent = parent.clone();
    Arc::new(move |action: Option<&ActionId>| read_fork_point(&dir, &parent, action))
}

/// The reader's read: the parent's own log to the point and model above.
fn read_fork_point(
    dir: &log::SessionDir,
    parent: &SessionId,
    action: Option<&ActionId>,
) -> Result<jobs::Forking, Failure> {
    // The parent's own log through the `log` crate, so the session path
    // stays there: a reader stops at the last complete line.
    let unreadable = |detail: String| {
        doors::failure(
            ErrorCode::ToolError,
            format!(
                "Cannot fork from the session log `{}`: {detail}",
                dir.path().display()
            ),
        )
    };
    let lines = log::read(dir.path()).map_err(|error| unreadable(error.to_string()))?;
    // The calling message and its `seq`, from the call's own
    // `tool_call_requested` or from the log's latest message.
    let calling = match action {
        Some(action) => {
            let call_seq = lines
                .iter()
                .filter(|line| {
                    line.kind == "tool_call_requested" && line.action_id.as_ref() == Some(action)
                })
                .filter_map(|line| line.seq)
                .max()
                .map(|seq| seq.0)
                .ok_or_else(|| unreadable("no `tool_call_requested` for the call".to_owned()))?;
            lines
                .iter()
                .filter(|line| {
                    line.kind == "assistant_message_started"
                        && line.seq.is_some_and(|seq| seq.0 < call_seq)
                })
                .max_by_key(|line| line.seq.map(|seq| seq.0))
                .ok_or_else(|| {
                    unreadable("no `assistant_message_started` before the call".to_owned())
                })?
        }
        None => lines
            .iter()
            .filter(|line| line.kind == "assistant_message_started" && line.seq.is_some())
            .max_by_key(|line| line.seq.map(|seq| seq.0))
            .ok_or_else(|| unreadable("no `assistant_message_started`".to_owned()))?,
    };
    let point = calling
        .seq
        .map(|seq| seq.0.saturating_sub(1))
        .ok_or_else(|| unreadable("no `assistant_message_started` before the call".to_owned()))?;
    // The calling message's own usage, written with the message's action
    // before the message completes, so it is on disk when the call runs.
    let model = lines
        .iter()
        .find(|line| {
            line.kind == "usage_recorded" && line.action_id.as_ref() == calling.action_id.as_ref()
        })
        .and_then(|line| line.payload.get("model"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| unreadable("no `usage_recorded` for the calling message".to_owned()))?;
    let model = model.to_owned();
    // The root the call ran inside, if any: a command root refuses the
    // call, while a link the log does not hold keeps the point and model.
    if let Some(action) = action {
        let mut current = action.clone();
        let mut seen = vec![current.clone()];
        while let Some(line) = lines.iter().find(|line| {
            line.kind == "tool_call_requested" && line.action_id.as_ref() == Some(&current)
        }) {
            let ran_by = line.payload.get("ran_by");
            let command = ran_by
                .and_then(|ran_by| ran_by.get("command_id"))
                .and_then(serde_json::Value::as_str);
            if let Some(command) = command {
                return Err(doors::failure(
                    ErrorCode::InvalidArguments,
                    format!("Cannot fork from a command: the call's root is command `{command}`."),
                ));
            }
            let outer = ran_by
                .and_then(|ran_by| ran_by.get("outer_action_id"))
                .and_then(serde_json::Value::as_str);
            let Some(outer) = outer else {
                break;
            };
            let next = ActionId(outer.to_owned());
            if seen.contains(&next) {
                break;
            }
            seen.push(next.clone());
            current = next;
        }
    }
    Ok(jobs::Forking {
        point: Point {
            session_id: parent.clone(),
            seq: Seq(point),
        },
        model,
    })
}

/// Reads the child's log boundary for a resume: the `seq` of the last
/// complete line of `<sessions>/<delegate session id>/events.jsonl`,
/// or the reason it could not be read, which names the log's path.
fn last_seq_reader(home: &Path) -> jobs::LastSeq {
    let home = home.to_path_buf();
    Arc::new(move |workspace, id| {
        let sessions = delegate_sessions(&home, workspace, id)?;
        log::last_seq(log::SessionDir::new(&sessions, id).path())
            .map(|seq| seq.map(|seq| seq.0))
            .map_err(|error| error.to_string())
    })
}

/// A Fiber worktree's path preserves the project key even after removal.
/// Existing workspaces resolve through git, including an explicitly selected repository.
/// A missing non-Fiber workspace uses the delegate's logged project instead.
/// The removed-worktree branch reads the key off the path and resolves it
/// through `log`; no session path is built here (`docs/architecture.md`,
/// rule 3).
fn delegate_sessions(home: &Path, workspace: &Path, id: &SessionId) -> Result<PathBuf, String> {
    let project = workspace
        .parent()
        .filter(|parent| parent.file_name() == Some(std::ffi::OsStr::new("worktrees")))
        .and_then(Path::parent)
        .filter(|project| {
            project.parent().is_some_and(|projects| {
                projects.file_name() == Some(std::ffi::OsStr::new("projects"))
                    && projects.parent() == Some(home)
            })
        });
    match project {
        Some(project) => {
            let key = project.file_name().and_then(|key| key.to_str());
            key.and_then(|key| log::sessions_dir_for_key(home, key))
                .ok_or_else(|| {
                    format!("{}: not a worktree of a Fiber project", workspace.display())
                })
        }
        None => {
            if !workspace.is_dir()
                && let Some((key, _)) = log::find_logged_session(home, id)
            {
                return log::sessions_dir_for_key(home, &key)
                    .ok_or_else(|| format!("{key}: invalid logged project key"));
            }
            Ok(log::sessions_dir(home, &doors::project(workspace)))
        }
    }
}

/// Reads only the child's durable first line; a bad record fails closed.
fn worktree_record_reader(home: &Path) -> jobs::WorktreeRecord {
    let home = home.to_path_buf();
    Arc::new(move |workspace, id| {
        let sessions = delegate_sessions(&home, workspace, id)?;
        let dir = log::SessionDir::new(&sessions, id);
        let started = log::started_first(dir.path())
            .map_err(|error| format!("{}: {error}", dir.events().display()))?;
        started
            .worktree_record()
            .map_err(|_| format!("{}: malformed worktree record", dir.events().display()))
    })
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
