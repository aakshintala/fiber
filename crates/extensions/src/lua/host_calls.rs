//! The host calls a suspended callback waits on (`docs/extensions.md`,
//! "Host calls"): admitting each request under the admission lock, and the
//! off-thread work that runs once `settle` suspends its callback. A test
//! pauses admission through the pause seam below.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, MutexGuard};
use std::thread;
use std::time::Instant;

use contract::RequestId;
use contract::events::ExtensionExec;
use contract::inbox::Delivery;
use contract::shapes::Process;

use crate::host::{self, Reply, Request, exec};
use crate::oauth::{self, Deliver, Holder};

use super::asks;
use super::hub::{Hub, Shared};
use super::schedule::Start;
use super::{CredentialFor, Target};

/// The suspended call a host request belongs to.
pub(super) struct Call<'t> {
    pub(super) id: u64,
    pub(super) target: &'t Target,
    pub(super) deadline: Option<Instant>,
}

/// What an admitted request leaves `settle` to do. The first two still hold
/// the admission lock and suspend under it; the rest released it, and suspend
/// under a fresh lock.
#[allow(
    clippy::large_enum_variant,
    reason = "an ask carries its unsent delivery to drop after unlocking; boxing would allocate on every host call"
)]
pub(super) enum Admitted<'a> {
    /// A `host.ask` raised: suspend waiting on `request`, then drop `unsent`
    /// after unlocking.
    Asked {
        shared: MutexGuard<'a, Shared>,
        request: RequestId,
        unsent: Option<Delivery>,
    },
    /// Off-thread work: suspend, then start `spawn`, still under the lock.
    Spawning {
        shared: MutexGuard<'a, Shared>,
        spawn: Spawn,
    },
    /// An answer ready at once: deliver it with no lock held, then suspend
    /// under a fresh lock.
    Replied(Reply),
    /// A listener or lock wait started under the lock, kept alive by its
    /// stop sender.
    Started(Sender<()>),
    /// A sleep: suspend until this instant.
    Sleep(Instant),
    /// Suspend with nothing to wait on (a sleep past the end of time).
    Nothing,
}

/// Off-thread work that starts only after its callback is suspended.
pub(super) struct Spawn {
    pub(super) builder: thread::Builder,
    pub(super) work: Box<dyn FnOnce() + Send>,
    pub(super) exec: bool,
}

/// Admits one suspended callback's host request under the admission lock.
/// Each arm returns what `settle` then suspends and starts: `Asked` and
/// `Spawning` keep the admission lock they carry, so the suspend and any
/// spawn happen under one lock hold; every other arm released the lock and
/// suspends under a fresh one. Off-thread work starts only after its callback
/// is suspended, so a reply arriving from work that answers in-process never
/// lands on an unsuspended id.
pub(super) fn admit<'a>(
    start: &Start,
    hub: &Arc<Hub>,
    mut shared: MutexGuard<'a, Shared>,
    call: Call<'_>,
    request: Request,
    driver: Option<Arc<dyn contract::extension::Drive>>,
    deliver: &Deliver,
) -> Admitted<'a> {
    let name = start.name.as_str();
    let home = start.home.as_path();
    let Call {
        id,
        target,
        deadline,
    } = call;
    match request {
        Request::Ask(interaction) => {
            if !shared.answerable || shared.router.closed() {
                // nobody_to_answer_returns_declined_at_once: as headless
                // approvals write no request.
                drop(shared);
                Admitted::Replied(host::Reply::Ask(contract::events::Answer::Declined {
                    declined: contract::shapes::True,
                }))
            } else {
                let request_id = asks::mint();
                let requested = contract::events::InteractionRequested {
                    request_id: request_id.clone(),
                    interaction,
                    action_ids: None,
                    extension: Some(name.to_owned()),
                    resumes: false,
                };
                let unsent = shared.raise(id, requested);
                // Suspended before anything can answer: the answer resumes
                // only from the loop's ack, after the drain.
                Admitted::Asked {
                    shared,
                    request: request_id,
                    unsent,
                }
            }
        }
        Request::Drive(request) => match driver {
            Some(driver) => {
                // The door runs the command elsewhere, as `host.http`
                // does: a driven `prompt` carrying an image blocks on
                // the image child, and the thread serves other work
                // meanwhile. Suspended before the spawn: the door answers
                // in-process, so its reply can arrive before a spawn
                // returns, and a reply for an unparked id is dropped.
                // No off-thread wait to cancel: the suspended callback's
                // deadline bounds the call, and a late answer is dropped.
                let answered = Arc::clone(hub);
                let extension = name.to_owned();
                let command = request.command;
                let args = request.args;
                Admitted::Spawning {
                    shared,
                    spawn: Spawn {
                        builder: thread::Builder::new().name(format!("drive {name}")),
                        work: Box::new(move || {
                            let ack = contract::inbox::Ack(Box::new(move |answer| {
                                answered.deliver(
                                    id,
                                    match answer {
                                        Ok(result) => Reply::Drive(Ok(result)),
                                        Err(rejection) => {
                                            Reply::Drive(Err((rejection.code, rejection.message)))
                                        }
                                    },
                                );
                            }));
                            driver.drive(&extension, &command, args, ack);
                        }),
                        exec: false,
                    },
                }
            }
            None => {
                // drive_without_a_driver_is_closing: only a sealed
                // extension sees this; it raises `closing`.
                drop(shared);
                Admitted::Replied(Reply::Drive(Err((
                    contract::ErrorCode::Closing,
                    "host.drive: the session is closing".to_owned(),
                ))))
            }
        },
        Request::Http(request) => {
            if let Some(script) = hub.host_script() {
                drop(shared);
                Admitted::Replied(Reply::Http(script.http(request.case_value())))
            } else {
                let http_deliver = Deliver::clone(deliver);
                Admitted::Spawning {
                    shared,
                    spawn: Spawn {
                        builder: thread::Builder::new().name(format!("http {name}")),
                        work: Box::new(move || http_deliver(Reply::Http(host::perform(&request)))),
                        exec: false,
                    },
                }
            }
        }
        Request::Callback { port, path } => {
            // The listener starts under the admission lock, so no cancel
            // or stop lands between admission and the bind; a failure is
            // delivered after releasing it.
            #[cfg(test)]
            settle_hook(target, Pause::Start);
            let started = if let Some(script) = hub.host_script() {
                let result = script
                    .oauth("callback", &serde_json::json!({}))
                    .map(|reply| {
                        reply
                            .get("query")
                            .and_then(serde_json::Value::as_object)
                            .into_iter()
                            .flat_map(|query| query.iter())
                            .filter_map(|(key, value)| {
                                value.as_str().map(|value| (key.clone(), value.to_owned()))
                            })
                            .collect()
                    });
                Err(Reply::Query(result))
            } else {
                oauth::listen(port, path.clone(), deliver, start.browser.as_ref())
            };
            drop(shared);
            match started {
                Ok(cancel) => Admitted::Started(cancel),
                Err(reply) => Admitted::Replied(reply),
            }
        }
        Request::Exec(request) => {
            let hub_exec = Arc::clone(hub);
            // A run inside a tool call is what the call's declared effects
            // were judged on, so it is not logged as `extension_exec`; one
            // from an effects function is, as the call is not admitted yet.
            let meta = (!matches!(target, Target::Tool(_))).then(|| ExecMeta {
                extension: name.to_owned(),
                program: request.program.clone(),
                args: request.args.clone(),
                cwd: request.cwd.clone(),
            });
            if let Some(script) = hub.host_script() {
                drop(shared);
                let result = script.exec(request.case_value()).map(|reply| {
                    let ran = exec::Ran {
                        exit_code: Some(reply.code),
                        signal: None,
                        stdout: reply.stdout.into_bytes(),
                        stderr: reply.stderr.into_bytes(),
                        timed_out: false,
                    };
                    send_exec(&hub_exec, meta.as_ref(), &ran);
                    ran
                });
                Admitted::Replied(Reply::Exec(result))
            } else {
                // The stop sender stays in the hub, so the stop reaches the
                // run without the Lua thread, and the run's end clears it.
                let cancel_rx = shared.register_exec(id);
                let clock = hub.clock_handle();
                let run_deliver = Deliver::clone(deliver);
                Admitted::Spawning {
                    shared,
                    spawn: Spawn {
                        builder: thread::Builder::new().name(format!("exec {name}")),
                        work: Box::new(move || {
                            let outcome = exec::run(&request, clock.as_ref(), deadline, cancel_rx);
                            // The group is empty or killed and drained past
                            // here: a cancelled call may return only now.
                            hub_exec.lock().finish_exec(id);
                            hub_exec.notify();
                            match &outcome {
                                Ok(ran) => send_exec(&hub_exec, meta.as_ref(), ran),
                                Err(failed) => {
                                    if let Some(ran) = &failed.ran {
                                        send_exec(&hub_exec, meta.as_ref(), ran);
                                    }
                                }
                            }
                            run_deliver(match outcome {
                                Ok(ran) => Reply::Exec(Ok(ran)),
                                Err(failed) => Reply::Exec(Err((failed.code, failed.message))),
                            });
                        }),
                        exec: true,
                    },
                }
            }
        }
        Request::Tool(request) => {
            // Only a running tool call anchors an inner call: every other
            // target, and a tool call with no runner, answers the error at
            // once, starting no tool work and writing no line.
            let runner = match target {
                Target::Tool(_) => shared.runners.get(&id).cloned(),
                Target::Command(_)
                | Target::Provider { .. }
                | Target::Hook { .. }
                | Target::Timer { .. }
                | Target::Search(_)
                | Target::Effects(_) => None,
            };
            match runner {
                Some(runner) => {
                    // Suspended before the spawn: the runner answers
                    // in-process when denied, so its reply can arrive
                    // before a spawn returns, and a reply for an unparked
                    // id is dropped. No off-thread wait to cancel: the
                    // suspended callback's deadline bounds the call, and a
                    // late answer is dropped.
                    let tool_deliver = Deliver::clone(deliver);
                    let extension = name.to_owned();
                    let tool = request.name;
                    let arguments = request.arguments;
                    Admitted::Spawning {
                        shared,
                        spawn: Spawn {
                            builder: thread::Builder::new().name(format!("tool {name}")),
                            work: Box::new(move || {
                                let reply = match runner.run(&extension, &tool, arguments) {
                                    Ok(completed) => Reply::Tool(host::tool::outcome(&completed)),
                                    Err(message) => Reply::Tool(Err(message)),
                                };
                                tool_deliver(reply);
                            }),
                            exec: false,
                        },
                    }
                }
                None => {
                    drop(shared);
                    Admitted::Replied(Reply::Tool(Err(
                        "host.tool: called outside a running tool call".to_owned(),
                    )))
                }
            }
        }
        Request::Lock => {
            // A command, hook, timer or tool holds no provider credential: calling
            // `refresh` there is an error in the calling code, raised as
            // a string.
            let no_credential = |what: &str| {
                crate::host::LockError::Arg(format!(
                    "host.oauth.refresh: a {what} has no provider credential to refresh"
                ))
            };
            // Only a provider credential starts a wait, under the admission
            // lock below, so no cancel or stop lands between admission and
            // the start. Every other target releases the lock first and
            // delivers its error after.
            match target {
                Target::Provider {
                    credential: Some(CredentialFor::Stored(pair)),
                    ..
                } => {
                    #[cfg(test)]
                    settle_hook(target, Pause::Start);
                    let started = oauth::lock(home, pair, deliver);
                    drop(shared);
                    match started {
                        Ok(cancel) => Admitted::Started(cancel),
                        Err(reply) => Admitted::Replied(reply),
                    }
                }
                Target::Provider {
                    credential: Some(CredentialFor::Login(slot)),
                    ..
                } => {
                    // A login holds no file: its slot is ready at once, and
                    // `fiber login` stores what it holds after the flow
                    // (`docs/model-routing.md`, "Logging in").
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Ok(Holder::Login(Arc::clone(slot)))))
                }
                Target::Provider {
                    name,
                    function,
                    credential: None,
                } => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(crate::host::LockError::Arg(format!(
                        "host.oauth.refresh: {name}.{function} has no credential to refresh; only credential() refreshes"
                    )))))
                }
                Target::Command(_) => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(no_credential("command"))))
                }
                Target::Hook { .. } => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(no_credential("hook"))))
                }
                Target::Timer { .. } => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(no_credential("timer"))))
                }
                Target::Tool(_) | Target::Effects(_) => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(no_credential("tool"))))
                }
                Target::Search(_) => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(no_credential("search backend"))))
                }
            }
        }
        Request::Sleep(d) => {
            drop(shared);
            // Past the end of time is no sleep.
            match hub.clock().now().checked_add(d) {
                Some(wake) => Admitted::Sleep(wake),
                None => Admitted::Nothing,
            }
        }
    }
}

/// The checkpoint a test observes: called with the target and stage reached.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Pause {
    Admission,
    Start,
    TimerDone,
}

/// A test's pause inside `settle`: called with the suspending target.
#[cfg(test)]
type SettleHook = Arc<dyn Fn(&Target, Pause) + Send + Sync>;

/// A test's pause inside `settle`, at admission and before an OAuth start,
/// or a signal after a timer callback completes. The hook runs without the
/// hub lock; tests can coordinate without sleeps. It sees only calls for the
/// tool it names; every other call passes through.
#[cfg(test)]
static SETTLE_HOOK: std::sync::Mutex<Option<SettleHook>> = std::sync::Mutex::new(None);

/// Runs `hook` at each `settle` admission point from now on.
#[cfg(test)]
pub(super) fn pause_settle(hook: SettleHook) {
    *SETTLE_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
}

/// Stops pausing at each `settle` admission point.
#[cfg(test)]
pub(super) fn unpause_settle() {
    *SETTLE_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

#[cfg(test)]
pub(super) fn settle_hook(target: &Target, pause: Pause) {
    let hook = SETTLE_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(hook) = hook {
        hook(target, pause);
    }
}

/// Sends a finished `host.exec` run to the session as its `extension_exec`
/// line, when it is logged: a scripted result reaches the session exactly
/// as a real run's (`docs/testing.md`, "Testing an extension").
fn send_exec(hub: &Hub, meta: Option<&ExecMeta>, ran: &exec::Ran) {
    if let Some(meta) = meta {
        hub.send(contract::inbox::Delivery::ExtensionExec(meta.exec(ran)));
    }
}

/// What a finished `host.exec` run is logged as: the extension, the program
/// with its arguments and working directory, and how it ended.
struct ExecMeta {
    extension: String,
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
}

impl ExecMeta {
    fn exec(&self, ran: &exec::Ran) -> ExtensionExec {
        ExtensionExec {
            extension: self.extension.clone(),
            program: self.program.clone(),
            args: self.args.clone(),
            cwd: self.cwd.display().to_string(),
            process: Process {
                exit_code: ran.exit_code,
                signal: ran.signal.clone(),
                timed_out: ran.timed_out,
            },
        }
    }
}
