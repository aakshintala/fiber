//! A repository's servers, offered and approved (`docs/mcp.md`, "A
//! repository's servers"): a server the repository declares starts only
//! when its current content is approved, runs from its pinned copy in
//! Fiber home, and one approved in the session's own offer starts before
//! that session's first model request.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use config::ProjectKey;
use contract::ErrorCode;
use contract::events::{Event, McpServerFailed, Notice, OfferedKind, ServerFailure};
use contract::shapes::Failure;
use contract::tool::Tool;

use super::RepositorySpec;
use doors::Declare;

#[cfg(test)]
#[path = "repository_servers_tests.rs"]
mod tests;

/// What `prepare` found for one repository server that is not starting yet.
#[derive(Debug, Clone)]
pub(crate) enum Waiting {
    /// Its current content has no approval: its hash and the repository's
    /// `required`.
    Unapproved {
        /// The current content's hash.
        hash: String,
        /// Whether the repository marks it `required`.
        required: bool,
    },
    /// This session skipped this content: kept waiting so the load
    /// re-reads it, since content changed since the skip is a new key.
    Skipped,
}

/// What `prepare` settled: the specs to start now, the failures for the
/// log, and the servers the post-offer load looks at again.
pub(crate) struct Prepared {
    /// The pinned specs to start with the session.
    pub ready: Vec<mcp::ServerSpec>,
    /// One per server that failed, in spec order.
    pub failed: Vec<McpServerFailed>,
    /// The first `required` failure, in spec order.
    pub required_failed: Option<McpServerFailed>,
    /// One per server that is not starting yet.
    pub waiting: Vec<(RepositorySpec, Waiting)>,
}

fn failed(name: &str, message: String) -> McpServerFailed {
    McpServerFailed {
        server: name.to_owned(),
        reason: ServerFailure::StartFailed,
        will_restart: false,
        error: crate::failed(ErrorCode::McpServerUnavailable, message),
    }
}

/// Sorts the repository's servers into starting now and waiting for the
/// post-offer load: an approved one becomes its pinned spec, an unapproved
/// one waits with its current hash, and a skipped one waits without
/// starting even when approved meanwhile. `skipped` holds the
/// `(name, content hash)` of each `mcp_server` content the session
/// skipped. A copy that cannot be made, and a declaration that drifted
/// since the configuration was read, fail that server only; a gathering
/// failure fails every repository server, so nothing starts unapproved.
pub(crate) fn prepare(
    home: &Path,
    project: &ProjectKey,
    workspace: &Path,
    clock: &Arc<dyn contract::clock::Clock>,
    skipped: &[r#loop::OfferKey],
    repository: Vec<RepositorySpec>,
) -> Prepared {
    let mut ready = Vec::new();
    let mut failures = Vec::new();
    let mut required_failed: Option<McpServerFailed> = None;
    let mut waiting = Vec::new();
    // A `required` failure stops the session starting; any other failure
    // joins the log.
    let mut push_failed = |spec: &RepositorySpec, failure: McpServerFailed| {
        if spec.spec.required && required_failed.is_none() {
            required_failed = Some(failure);
        } else {
            failures.push(failure);
        }
    };
    let skipped_set: BTreeSet<(String, String)> = skipped
        .iter()
        .filter(|key| key.kind == OfferedKind::McpServer)
        .map(|key| (key.name.clone(), key.hash.clone()))
        .collect();
    let approvals = match extensions::repository_servers(
        home,
        project,
        workspace,
        Arc::clone(clock),
        &skipped_set,
    ) {
        Ok(approvals) => approvals,
        Err(error) => {
            for spec in &repository {
                push_failed(
                    spec,
                    failed(
                        &spec.spec.name,
                        format!(
                            "The MCP server `{}` was not started: \
                             the repository's servers could not be read: {error}.",
                            spec.spec.name,
                        ),
                    ),
                );
            }
            return Prepared {
                ready,
                failed: failures,
                required_failed,
                waiting,
            };
        }
    };
    for spec in repository {
        match approvals.get(&spec.spec.name) {
            None => push_failed(
                &spec,
                failed(
                    &spec.spec.name,
                    format!(
                        "The MCP server `{}` was not started: \
                         the repository no longer declares it. \
                         Start a new session to use it.",
                        spec.spec.name,
                    ),
                ),
            ),
            Some(Ok(extensions::ServerApproval::Approved(approved))) => {
                match pinned_spec(&spec, approved) {
                    Ok(pinned) => ready.push(pinned),
                    Err(message) => {
                        let name = spec.spec.name.clone();
                        push_failed(&spec, failed(&name, message));
                    }
                }
            }
            Some(Ok(extensions::ServerApproval::Unapproved { hash, required })) => {
                waiting.push((
                    spec,
                    Waiting::Unapproved {
                        hash: hash.clone(),
                        required: *required,
                    },
                ));
            }
            // The current key is in `skipped`, decided before any approval
            // read or copy, so a copy error never defeats the skip. The
            // load drops it with no line; content changed since the skip
            // is a new key the load re-reads.
            Some(Ok(extensions::ServerApproval::Skipped)) => {
                waiting.push((spec, Waiting::Skipped));
            }
            Some(Err(error)) => {
                let message = format!(
                    "The MCP server `{}` was not started: \
                     its pinned copy could not be made: {error}.",
                    spec.spec.name,
                );
                let name = spec.spec.name.clone();
                push_failed(&spec, failed(&name, message));
            }
        }
    }
    Prepared {
        ready,
        failed: failures,
        required_failed,
        waiting,
    }
}

/// The spec that runs the approved copy: the merged command, arguments and
/// environment mapped string by string through the approved server's
/// frozen resolutions. A bare-word command is looked up on the `PATH` and
/// never pinned. Every repository-supplied value must still equal the
/// approved declaration's: a changed one is not started. A person-layer
/// value is the person's own and never needs to match. `Err` is the whole
/// failure message.
pub(crate) fn pinned_spec(
    candidate: &RepositorySpec,
    approved: &extensions::ApprovedServer,
) -> Result<mcp::ServerSpec, String> {
    let name = &candidate.spec.name;
    // The approval records the `{"name", "entry"}` declaration: the
    // drift check reads the repository's checked entry inside it.
    let entry = approved.entry.get("entry");
    let drifted = format!(
        "The MCP server `{name}` was not started: \
         the repository's declaration of it changed after this session read \
         its configuration. Start a new session to use it."
    );
    if let Some(command) = candidate.fields.command.as_ref()
        && entry.and_then(|entry| entry.get("command")) != Some(command)
    {
        return Err(drifted);
    }
    if let Some(args) = candidate.fields.args.as_ref()
        && entry.and_then(|entry| entry.get("args")) != Some(args)
    {
        return Err(drifted);
    }
    if let Some(url) = candidate.fields.url.as_ref()
        && entry.and_then(|entry| entry.get("url")) != Some(url)
    {
        return Err(drifted);
    }
    for (key, value) in &candidate.fields.env {
        if entry
            .and_then(|entry| entry.get("env"))
            .and_then(|env| env.get(key))
            != Some(value)
        {
            return Err(drifted);
        }
    }
    let stdio = match &candidate.spec.transport {
        // A remote server pins nothing: its declaration runs as declared.
        mcp::Transport::Http { .. } => return Ok(candidate.spec.clone()),
        mcp::Transport::Stdio(stdio) => stdio,
    };
    let command = if stdio.command.contains('/') {
        approved.arg(&stdio.command)
    } else {
        stdio.command.clone()
    };
    let args: Vec<String> = stdio.args.iter().map(|arg| approved.arg(arg)).collect();
    // What the guard re-checks at every spawn: the command when it names a
    // path, and the merged arguments. A string the declaration named is
    // frozen and never given to it; a bare-word command is looked up on
    // the `PATH`.
    let mut given = Vec::new();
    if stdio.command.contains('/') {
        given.push(stdio.command.clone());
    }
    given.extend(stdio.args.iter().cloned());
    let guard = approved.guard(&given);
    let guard_name = name.clone();
    let mut spec = candidate.spec.clone();
    match &mut spec.transport {
        mcp::Transport::Stdio(pinned) => {
            pinned.run = Some(mcp::Program {
                command,
                args,
                guard: Arc::new(move || {
                    guard.check().map_err(|error| {
                        let why = if let extensions::Error::Pin { why, .. } = &error {
                            why.clone()
                        } else {
                            error.to_string()
                        };
                        format!("The MCP server `{guard_name}` was not started: {why}")
                    })
                }),
            });
        }
        // A remote spec returned above, so its clone is never here.
        mcp::Transport::Http { .. } => {}
    }
    Ok(spec)
}

/// The servers the post-offer load may still start: everything `prepare`
/// left waiting, with what starting them needs.
pub(crate) struct Later {
    pub(crate) home: PathBuf,
    pub(crate) project: ProjectKey,
    pub(crate) workspace: PathBuf,
    pub(crate) artifacts: PathBuf,
    pub(crate) images: Arc<dyn contract::images::Images>,
    pub(crate) cache: PathBuf,
    pub(crate) clock: Arc<dyn contract::clock::Clock>,
    pub(crate) waiting: Vec<RepositorySpec>,
    pub(crate) servers: Arc<mcp::Servers>,
}

impl Later {
    /// The post-offer load: what the session loads once its offer step has
    /// run, before its first preamble. It holds the waiting specs in a
    /// lock and takes them on its first call; a second call returns an
    /// empty `Approved`.
    pub(crate) fn into_load(
        self,
        declare: Declare,
        caps: r#loop::ResultCaps,
    ) -> r#loop::LoadApproved {
        let waiting = Mutex::new(Some(self));
        Arc::new(move |outcome: &r#loop::OfferOutcome| {
            let later = support::lock(&waiting).take();
            match later {
                None => Ok(r#loop::Approved::default()),
                Some(later) => later.load(outcome, &declare, &caps),
            }
        })
    }

    /// Starts what the offer settled: content approved since the process
    /// started, and approved meanwhile by `fiber approve`. Content the
    /// offer step already settled is dropped with no new line; content it
    /// never saw is reported as an unapproved item. The required checks
    /// run before any start, so a failure starts no process. The servers'
    /// prompts are not added to this process's rows: they run from the
    /// next process start.
    fn load(
        self,
        outcome: &r#loop::OfferOutcome,
        declare: &Declare,
        caps: &r#loop::ResultCaps,
    ) -> Result<r#loop::Approved, Failure> {
        let Self {
            home,
            project,
            workspace,
            artifacts,
            images,
            cache,
            clock,
            waiting,
            servers,
        } = self;
        if waiting.is_empty() {
            return Ok(r#loop::Approved::default());
        }
        let prepared = prepare(
            &home,
            &project,
            &workspace,
            &clock,
            &outcome.skipped,
            waiting,
        );
        if let Some(failure) = prepared.required_failed {
            return Err(Failure {
                code: ErrorCode::McpRequiredServerFailed,
                message: failure.error.message.clone(),
                retry_after_ms: None,
                provider: None,
            });
        }
        let mut events = Vec::new();
        for (spec, state) in &prepared.waiting {
            let Waiting::Unapproved { hash, required } = state else {
                // Skipped for this session, even when approved meanwhile.
                continue;
            };
            let current = r#loop::OfferKey {
                kind: OfferedKind::McpServer,
                name: spec.spec.name.clone(),
                hash: hash.clone(),
            };
            if outcome.skipped.contains(&current) || outcome.reported.contains(&current) {
                continue;
            }
            // Content the offer step never saw: the declaration or a named
            // file changed after its last check, and has no approval.
            if *required {
                return Err(Failure {
                    code: ErrorCode::McpServerUnapproved,
                    message: format!(
                        "The repository requires the MCP server `{}` \
                         whose declaration or files changed after this \
                         session's offer, so nobody approved it. \
                         Run `fiber approve` in the repository, \
                         then start a new session.",
                        spec.spec.name,
                    ),
                    retry_after_ms: None,
                    provider: None,
                });
            }
            events.push(Event::Notice(Notice {
                code: ErrorCode::RepositoryCodeSkipped,
                message: format!(
                    "The repository's MCP server `{}` changed after this \
                     session's offer, so it was not started. \
                     Run `fiber approve` in the repository to approve it, \
                     then start a new session.",
                    spec.spec.name,
                ),
                extension: None,
            }));
        }
        let mut started = mcp::start_observed(
            prepared.ready,
            &workspace,
            &artifacts,
            Arc::clone(&images),
            &cache,
            &clock,
            env!("CARGO_PKG_VERSION"),
            // The original session's context follows its late servers:
            // structs keep their shape, so it is read from the servers.
            servers.timing(),
            servers.debug_sink(),
        );
        if let Some(failure) = started.required_failed {
            started.servers.stop();
            return Err(Failure {
                code: ErrorCode::McpRequiredServerFailed,
                message: failure.error.message.clone(),
                retry_after_ms: None,
                provider: None,
            });
        }
        servers.adopt(started.servers);
        // The session already declares `mcp_resources`: the post-offer
        // tool reaches the newly adopted servers, and no second
        // registration is ever offered for replacement.
        if servers.resources_declared() {
            started
                .tools
                .retain(|(_, tool)| tool.definition().name != mcp::RESOURCES_TOOL);
            started
                .infos
                .retain(|info| info.name != mcp::RESOURCES_TOOL);
        }
        // The names the session held before the load stand: an
        // extension's replacement keeps its `tools` row, so the started
        // rows those names held are dropped and never declared. The
        // tools themselves still load, and the loop leaves the
        // registered names alone.
        started
            .infos
            .retain(|info| !outcome.held.contains(&info.name));
        for info in &started.infos {
            declare(&info.name, Some(info.clone()));
        }
        let tools: Vec<(String, Arc<dyn Tool>)> = r#loop::capped(started.tools, caps);
        events.extend(
            prepared
                .failed
                .into_iter()
                .chain(started.failed)
                .map(Event::McpServerFailed),
        );
        Ok(r#loop::Approved { tools, events })
    }
}
