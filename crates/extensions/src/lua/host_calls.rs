//! The host calls a suspended callback waits on (`docs/extensions.md`,
//! "Host calls"): admitting each request under the admission lock, and the
//! off-thread work that runs once `settle` suspends its callback. A test
//! pauses admission through the pause seam below.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::Instant;
use support::lock;

use contract::RequestId;
use contract::clock::Wake;
use contract::events::ExtensionExec;
use contract::inbox::Delivery;
use contract::shapes::Process;
use contract::tool::Cancel;

use crate::host::script::{ExecEnd, ExecReply};
use crate::host::{self, Reply, Request, exec};
use crate::oauth::{self, Deliver, Holder};

use super::asks;
use super::hub::{Hub, Shared};
use super::schedule::Start;
use super::{CredentialFor, Target};

/// The end of one call's `host.model` run. The admission registers it;
/// the worker holds it in `Bound::cancel`. Ending it wakes subscribers
/// outside this signal's lock: a subscriber must never take the hub lock,
/// because the hub ends it while holding that lock.
#[derive(Default)]
pub(super) struct ModelCancel {
    state: Mutex<(bool, Vec<Weak<dyn Wake>>)>,
}

impl ModelCancel {
    /// Sets the cancel: the callback ended, the call was cancelled, or the
    /// hub stopped or sealed. Idempotent; wakes subscribers after the
    /// flag is set, so a subscriber that arrives after it is never woken.
    pub(super) fn end(&self) {
        let subscribers = {
            let mut state = lock(&self.state);
            if state.0 {
                return;
            }
            state.0 = true;
            std::mem::take(&mut state.1)
        };
        for subscriber in subscribers.into_iter().filter_map(|weak| weak.upgrade()) {
            subscriber.wake();
        }
    }
}

impl Cancel for ModelCancel {
    fn is_cancelled(&self) -> bool {
        lock(&self.state).0
    }

    fn subscribe(&self, wake: Weak<dyn Wake>) {
        let mut state = lock(&self.state);
        if !state.0 {
            state.1.push(wake);
        }
    }
}

impl Shared {
    /// Registers the `host.model` run of the call `id`, admitted by
    /// [`Hub::admit`] under the same lock hold, and returns its cancel.
    pub(super) fn register_model(&mut self, id: u64) -> Arc<ModelCancel> {
        let cancel = Arc::new(ModelCancel::default());
        self.models.insert(id, Arc::clone(&cancel));
        cancel
    }

    /// Ends the `host.model` run of the call `id`, if it still holds one:
    /// its callback ended, the call was cancelled, or the hub stopped.
    pub(super) fn end_model(&mut self, id: u64) {
        if let Some(cancel) = self.models.get(&id) {
            cancel.end();
        }
    }

    /// Drops the `host.model` registration of the call `id`: its worker
    /// returned, so no later end may fire for it.
    pub(super) fn finish_model(&mut self, id: u64) {
        self.models.remove(&id);
    }
}

/// The suspended call a host request belongs to.
pub(super) struct Call<'t> {
    pub(super) id: u64,
    pub(super) target: &'t Target,
    pub(super) deadline: Option<Instant>,
    /// The snapshotted observation for an eligible provider HTTP request:
    /// owned context the admitted worker observes with once it executes.
    pub(super) http: Option<host::HttpObservation>,
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
#[allow(clippy::too_many_arguments, reason = "one admit for every host call")]
pub(super) fn admit<'a>(
    start: &Start,
    hub: &Arc<Hub>,
    mut shared: MutexGuard<'a, Shared>,
    call: Call<'_>,
    request: Request,
    driver: Option<Arc<dyn contract::extension::Drive>>,
    model: Option<Arc<dyn contract::model::HostModel>>,
    delegate: Option<Arc<dyn contract::extension::HostDelegate>>,
    deliver: &Deliver,
) -> Admitted<'a> {
    let name = start.name.as_str();
    let home = start.home.as_path();
    let Call {
        id,
        target,
        deadline,
        http,
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
                let worker = Arc::clone(hub);
                Admitted::Spawning {
                    shared,
                    spawn: Spawn {
                        builder: thread::Builder::new().name(format!("http {name}")),
                        work: Box::new(move || {
                            // The rendezvous: the worker crosses the hub
                            // lock before touching the observer or the
                            // transport, so nothing is read or written
                            // while settle still holds the spawning
                            // admission guard. The lock is released before
                            // the request runs.
                            drop(worker.lock());
                            #[cfg(test)]
                            if let Some(observation) = http.as_ref() {
                                await_observed_release(&observation.provider);
                            }
                            http_deliver(Reply::Http(match http {
                                Some(observation) => host::perform_observed(&request, observation),
                                None => host::perform(&request),
                            }));
                        }),
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
                // The hang arm below keeps the admission lock held into its
                // spawn, so no stop lands between the match and the
                // registration; every other arm drops it before delivering.
                let case_value = request.case_value();
                let (index, outcome) = script.exec(case_value.clone());
                match outcome {
                    Ok(ExecReply::Hang) => match deadline {
                        Some(until) => {
                            // A process that never exits: the worker parks on
                            // the case clock until the callback's deadline,
                            // as a real host.exec parks on the injected
                            // clock. The scheduler answers the callback's
                            // timeout, so the worker delivers no reply and
                            // writes no exec line.
                            script.hang_matched(index, &case_value, until);
                            let cancel_rx = shared.register_exec(id);
                            let clock = hub.clock_handle();
                            let worker = Arc::clone(&hub_exec);
                            Admitted::Spawning {
                                shared,
                                spawn: Spawn {
                                    builder: thread::Builder::new().name(format!("exec {name}")),
                                    work: Box::new(move || {
                                        script.park_hang(clock.as_ref(), until, &cancel_rx);
                                        worker.lock().finish_exec(id);
                                        worker.notify();
                                    }),
                                    exec: true,
                                },
                            }
                        }
                        None => {
                            drop(shared);
                            Admitted::Replied(Reply::Exec(Err((
                                contract::ErrorCode::IoFailed,
                                "host.exec: a scripted hang needs a callback deadline".to_owned(),
                            ))))
                        }
                    },
                    Ok(ExecReply::Completed {
                        end,
                        stdout,
                        stderr,
                    }) => {
                        drop(shared);
                        let (exit_code, signal) = match end {
                            ExecEnd::Code(code) => (Some(code), None),
                            ExecEnd::Signal(number) => {
                                (None, Some(support::group::signal_name(number)))
                            }
                        };
                        let ran = exec::Ran {
                            exit_code,
                            signal,
                            stdout: stdout.into_bytes(),
                            stderr: stderr.into_bytes(),
                            timed_out: false,
                        };
                        send_exec(&hub_exec, meta.as_ref(), &ran);
                        Admitted::Replied(Reply::Exec(Ok(ran)))
                    }
                    Err(failed) => {
                        drop(shared);
                        Admitted::Replied(Reply::Exec(Err(failed)))
                    }
                }
            } else {
                // The stop sender stays in the hub, so the stop reaches the
                // run without the Lua thread, and the run's end clears it.
                let cancel_rx = shared.register_exec(id);
                let clock = hub.clock_handle();
                let run_deliver = Deliver::clone(deliver);
                #[cfg(test)]
                let target = target.clone();
                Admitted::Spawning {
                    shared,
                    spawn: Spawn {
                        builder: thread::Builder::new().name(format!("exec {name}")),
                        work: Box::new(move || {
                            let outcome = exec::run(&request, clock.as_ref(), deadline, cancel_rx);
                            #[cfg(test)]
                            settle_hook(&target, Pause::ExecEnding);
                            match &outcome {
                                Ok(ran) => send_exec(&hub_exec, meta.as_ref(), ran),
                                Err(failed) => {
                                    if let Some(ran) = &failed.ran {
                                        send_exec(&hub_exec, meta.as_ref(), ran);
                                    }
                                }
                            }
                            // Queue the line before releasing the callback or
                            // the shutdown barrier: both may seal its router.
                            hub_exec.lock().finish_exec(id);
                            hub_exec.notify();
                            #[cfg(test)]
                            settle_hook(&target, Pause::ExecFinished);
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
            // An admitted command or running tool call anchors an inner call: every other
            // target, and a tool call with no runner, answers the error at
            // once, starting no tool work and writing no line.
            let runner = match target {
                Target::Tool(_) | Target::Command(_) => shared.runners.get(&id).cloned(),
                Target::Provider { .. }
                | Target::Hook { .. }
                | Target::Watcher { .. }
                | Target::Timer { .. }
                | Target::Search(_)
                | Target::Delegate { .. }
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
        Request::Model(spec) => {
            // A command, tool, hook or timer callback may call the
            // session's model; an effects function, a provider function or
            // a search backend answers now, as the tool arm does for a
            // target with no runner.
            match target {
                Target::Tool(_)
                | Target::Command(_)
                | Target::Hook { .. }
                | Target::Watcher { .. }
                | Target::Delegate { .. }
                | Target::Timer { .. } => {
                    match model {
                        Some(model) => {
                            // The registration stays in the hub, so the
                            // end of the callback or the hub reaches the
                            // worker without the Lua thread: the worker
                            // drops it on its return.
                            let cancel = shared.register_model(id);
                            let worker = Arc::clone(hub);
                            let model_deliver = Deliver::clone(deliver);
                            let extension = name.to_owned();
                            let clock = hub.clock_handle();
                            let request = spec;
                            Admitted::Spawning {
                                shared,
                                spawn: Spawn {
                                    builder: thread::Builder::new().name(format!("model {name}")),
                                    work: Box::new(move || {
                                        // One `usage_recorded` line per
                                        // attempt, in order: the hub routes
                                        // each unless it is sealed, and
                                        // drops a sealed one with the hub,
                                        // as `ExtensionExec` is dropped.
                                        let lines = {
                                            let worker = Arc::clone(&worker);
                                            Arc::new(
                                                move |recorded: contract::events::UsageRecorded| {
                                                    worker.send(Delivery::ExtensionUsage(recorded));
                                                },
                                            )
                                        };
                                        let bound = contract::model::Bound {
                                            deadline,
                                            clock,
                                            cancel,
                                            lines,
                                        };
                                        let reply = match model.call(&extension, request, bound) {
                                            Ok(answered) => Reply::Model(Ok(answered)),
                                            Err(failed) => {
                                                Reply::Model(Err((failed.code, failed.message)))
                                            }
                                        };
                                        // The run is over: its registration
                                        // goes before the late reply, so
                                        // no late end may fire for it.
                                        worker.lock().finish_model(id);
                                        worker.notify();
                                        model_deliver(reply);
                                    }),
                                    exec: false,
                                },
                            }
                        }
                        None => {
                            // No model is injected: only a sealed hub
                            // sees this; it answers `closing`, as
                            // `host.drive` does.
                            drop(shared);
                            Admitted::Replied(Reply::Model(Err((
                                contract::ErrorCode::Closing,
                                "host.model: the session is closing".to_owned(),
                            ))))
                        }
                    }
                }
                Target::Provider { .. } | Target::Search(_) | Target::Effects(_) => {
                    drop(shared);
                    Admitted::Replied(Reply::Model(Err((
                        contract::ErrorCode::InvalidArguments,
                        "host.model: not available here".to_owned(),
                    ))))
                }
            }
        }
        Request::Delegate {
            arguments,
            handle,
            timeout,
        } => {
            // The target comes first, as the `Request::Model` arm does: a
            // provider function, a search function or an effects function
            // never reaches the service. A delegate `run` may start
            // another delegate, as a tool's `run` may.
            match target {
                Target::Provider { .. }
                | Target::Watcher { .. }
                | Target::Search(_)
                | Target::Effects(_) => {
                    drop(shared);
                    Admitted::Replied(Reply::Delegate(Err((
                        contract::ErrorCode::InvalidArguments,
                        "host.delegate: not available here".to_owned(),
                    ))))
                }
                Target::Tool(_)
                | Target::Command(_)
                | Target::Hook { .. }
                | Target::Delegate { .. }
                | Target::Timer { .. } => match delegate {
                    // No service is injected: only a sealed hub sees this;
                    // it answers `closing`, as `host.drive` does.
                    None => {
                        drop(shared);
                        Admitted::Replied(Reply::Delegate(Err((
                            contract::ErrorCode::Closing,
                            "host.delegate: the session is closing".to_owned(),
                        ))))
                    }
                    Some(service) => {
                        if shared.abandoned {
                            // An abandoned hub's leaked thread may still run
                            // Lua: its start spawns nothing.
                            drop(shared);
                            Admitted::Replied(Reply::Delegate(Err((
                                contract::ErrorCode::Closing,
                                "host.delegate: this extension was stopped".to_owned(),
                            ))))
                        } else if !shared.router.has_inbox() {
                            // Before the session's loop wires the inbox a start
                            // would get `NoInbox`: fail closed instead, before
                            // the service is called.
                            drop(shared);
                            Admitted::Replied(Reply::Delegate(Err((
                                contract::ErrorCode::Closing,
                                "host.delegate: the session is not running yet".to_owned(),
                            ))))
                        } else {
                            // Suspended before the spawn: the service calls
                            // `bind` under its own start gate, so the reply can
                            // arrive before a spawn returns, and a reply for an
                            // unparked id is dropped. No off-thread wait to
                            // cancel: the suspended callback's deadline bounds
                            // the call, and a late answer is dropped.
                            let worker = Arc::clone(hub);
                            let delegate_deliver = Deliver::clone(deliver);
                            let extension = name.to_owned();
                            #[cfg(test)]
                            let target = target.clone();
                            Admitted::Spawning {
                                shared,
                                spawn: Spawn {
                                    builder: thread::Builder::new()
                                        .name(format!("delegate {name}")),
                                    work: Box::new(move || {
                                        let mut bound: Option<contract::JobId> = None;
                                        let mut bind = |id: &contract::JobId| {
                                            bound = Some(id.clone());
                                            worker.bind_delegate(id.clone(), handle, timeout);
                                        };
                                        match service.start(&extension, &arguments, &mut bind) {
                                            Ok(id) => {
                                                // The post-start check: an
                                                // abandon between `bind` and
                                                // the job's record found no
                                                // job to stop, so end it here.
                                                worker.cancel_delegate_if_abandoned(&service, &id);
                                                #[cfg(test)]
                                                settle_hook(&target, Pause::DelegateChecked);
                                                delegate_deliver(Reply::Delegate(Ok(id)));
                                            }
                                            Err(failed) => {
                                                // A start that fails after
                                                // `bind` leaves no binding
                                                // for an abandon to cancel.
                                                if let Some(id) = bound {
                                                    worker.unbind_delegate(&id);
                                                }
                                                delegate_deliver(Reply::Delegate(Err((
                                                    failed.code,
                                                    failed.message,
                                                ))));
                                            }
                                        }
                                    }),
                                    exec: false,
                                },
                            }
                        }
                    }
                },
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
                Target::Watcher { .. } => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(no_credential("watcher"))))
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
                Target::Delegate { .. } => {
                    drop(shared);
                    Admitted::Replied(Reply::Lock(Err(no_credential("delegate run"))))
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
pub(crate) enum Pause {
    Admission,
    ExecEnding,
    ExecFinished,
    Start,
    TimerDone,
    /// After a delegate start's post-start check, before its reply is delivered.
    DelegateChecked,
}

/// A test's pause inside `settle`: called with the suspending target.
#[cfg(test)]
type SettleHook = Arc<dyn Fn(&Target, Pause) + Send + Sync>;

/// A test's pause inside `settle`, at admission and before an OAuth start,
/// or a signal after a timer callback completes. The hook runs without the
/// hub lock; tests can coordinate without sleeps. It sees only calls for the
/// tool it names; every other call passes through. `cfg(test)`: `settle` is
/// entered only from the extension thread, so no public API of
/// `LuaExtension` reaches the interleavings between the admission decision
/// and the OAuth start or the timer's completion, and the pause is the only
/// way to hold them there.
#[cfg(test)]
static SETTLE_HOOK: std::sync::Mutex<Option<SettleHook>> = std::sync::Mutex::new(None);

/// Runs `hook` at each `settle` admission point from now on.
#[cfg(test)]
pub(crate) fn pause_settle(hook: SettleHook) {
    *lock(&SETTLE_HOOK) = Some(hook);
}

/// Stops pausing at each `settle` admission point.
#[cfg(test)]
pub(crate) fn unpause_settle() {
    *lock(&SETTLE_HOOK) = None;
}

#[cfg(test)]
pub(super) fn settle_hook(target: &Target, pause: Pause) {
    let hook = lock(&SETTLE_HOOK).clone();
    if let Some(hook) = hook {
        hook(target, pause);
    }
}

/// A test's pause inside an observed HTTP worker, after the hub-lock
/// rendezvous and before any stamp, read or write. `cfg(test)`: no public
/// API can freeze that post-spawn interleaving, so the rendezvous proof
/// needs this pause. Scoped by provider name, so parallel tests with
/// distinct providers pass through. The worker blocks holding no lock, so
/// the test stays free to assert and release it; dropping the release
/// sender frees the worker, so a failing test never hangs the suite.
#[cfg(test)]
static OBSERVED_PAUSE: std::sync::Mutex<Option<ObservedPause>> = std::sync::Mutex::new(None);

#[cfg(test)]
struct ObservedPause {
    provider: String,
    arrived: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

/// Pauses the next observed HTTP worker of `provider` before it stamps or
/// writes, returning the arrival signal and the release. Narrow: only
/// that provider's observed workers wait, so other tests' workers pass
/// through while it is armed. Fires once per arming; disarm an unused
/// arming with [`unpause_observed_execution`].
#[cfg(test)]
pub(crate) fn pause_observed_execution(
    provider: &str,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *lock(&OBSERVED_PAUSE) = Some(ObservedPause {
        provider: provider.to_owned(),
        arrived: arrived_tx,
        release: release_rx,
    });
    (arrived_rx, release_tx)
}

/// Stops pausing observed HTTP workers.
#[cfg(test)]
pub(crate) fn unpause_observed_execution() {
    *lock(&OBSERVED_PAUSE) = None;
}

/// Waits for the test's release when an observed worker of the paused
/// provider reaches its pause, and passes everything else through. The
/// worker blocks holding no lock.
#[cfg(test)]
fn await_observed_release(provider: &str) {
    let gate = lock(&OBSERVED_PAUSE).take_if(|pause| pause.provider == provider);
    if let Some(pause) = gate {
        let _ignored = pause.arrived.send(());
        let _ignored = pause.release.recv();
    }
}
/// line, when it is logged: a scripted result reaches the session exactly
/// as a real run's (`docs/testing.md`, "Testing an extension").
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

#[cfg(test)]
#[path = "host_calls_tests.rs"]
mod tests;
