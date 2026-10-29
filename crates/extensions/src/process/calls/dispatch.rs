use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use serde_json::Value;

use crate::process::registry::in_callback_message;
use crate::process::{
    ProcessExtension, Pumped, RegisterOutcome, answer_off, answer_on, calls, context, lifecycle,
    oauth, requests, transport,
};

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
impl ProcessExtension {
    /// Handles one line or close from the child, answering it. A close
    /// records the death once per child spawn, retiring the generation
    /// so late calls and replies read stale.
    pub(crate) fn pump_once(&self) -> Pumped {
        // The pump holds its own child: the lock drops before the
        // blocking receive, so a restart swaps the live one freely.
        let live = Arc::clone(&support::lock(&self.shared.transport));
        match live.recv() {
            transport::Incoming::Line(bytes) => {
                self.handle_line(&bytes, &live);
                Pumped::Handled
            }
            transport::Incoming::Closed(reason) => {
                lifecycle::note_death(&self.shared, live.spawn_id());
                Pumped::Closed(reason)
            }
        }
    }

    /// Handles one complete child line, answering it.
    fn handle_line(&self, bytes: &[u8], live: &transport::Transport) {
        match transport::parse_line(bytes) {
            transport::ParsedLine::Call {
                id,
                call,
                args,
                request_id,
            } => self.handle_call(id, call, args, request_id),
            transport::ParsedLine::Ready { id } => {
                self.answer_accepted(&id, None);
                self.shared
                    .ready
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                lifecycle::note_ready(&self.shared);
                self.shared.ready_park.bump();
            }
            transport::ParsedLine::Reply {
                id,
                request_id,
                answer,
            } => {
                let outcome = match answer {
                    transport::ReplyAnswer::Value(result) => {
                        requests::Outcome::Answered(requests::Answer::Value(result))
                    }
                    transport::ReplyAnswer::Failure { code, message } => {
                        requests::Outcome::Answered(requests::Answer::Failure { code, message })
                    }
                };
                match self
                    .shared
                    .table
                    .settle_with_before_wake(&request_id, outcome, || {
                        self.answer_accepted_on(live, &id, None);
                    }) {
                    requests::Settled::Completed => {}
                    requests::Settled::Stale => self.answer_stale(&id),
                }
            }
            transport::ParsedLine::Forward { id, command, args } => {
                self.drive_command(id, command, args);
            }
            transport::ParsedLine::Unknown { id, command } => {
                self.answer_rejected(&id, ErrorCode::UnknownCommand, not_built(&command));
            }
            transport::ParsedLine::BadReply { id } => {
                self.answer_rejected(
                    &id,
                    ErrorCode::InvalidArguments,
                    "a `reply` answers with `result` or `error`.".to_owned(),
                );
            }
            transport::ParsedLine::Unfit { id } => {
                self.answer_rejected(
                    &id,
                    ErrorCode::InvalidArguments,
                    transport::UNFIT.to_owned(),
                );
            }
            transport::ParsedLine::Malformed { id } => {
                self.answer_rejected(&id, ErrorCode::Malformed, transport::MALFORMED.to_owned());
            }
            transport::ParsedLine::Ignored => {}
        }
    }

    /// Answers one `call`: registrations through the registry before
    /// `ready` and refused after it, `host.drive` through the `Drive`
    /// seam, and host and state calls through the injected services. A
    /// call naming a request that is no longer pending is rejected
    /// `stale_request` and runs nothing. A failed package stays failed:
    /// later calls fail with the first failure, without touching the
    /// registry again.
    fn handle_call(
        &self,
        id: contract::CommandId,
        call: String,
        args: Vec<Value>,
        request_id: Option<contract::RequestId>,
    ) {
        if let Some((code, message)) = support::lock(&self.shared.registry).failure() {
            self.answer_rejected(&id, code, message);
            return;
        }
        let context = match request_id {
            Some(request) => match self.shared.table.context(&request) {
                Some(context) => context,
                None => {
                    self.answer_stale(&id);
                    return;
                }
            },
            None => self.call_context(),
        };
        // A run outside a session carries reason `call`: the driver
        // refuses without a session, and only the Call-case set runs.
        if support::lock(&self.shared.context).is_call() && call == "host.drive" {
            let (code, message) = calls::not_available(&call);
            self.answer_rejected(&id, code, message);
            return;
        }
        match calls::route_call(&call, args) {
            Ok(calls::Route::Register { call, args }) => {
                if !context.registration_allowed() {
                    // Refused without touching the registry: no
                    // registration stands, and no notice grows.
                    self.answer_rejected(
                        &id,
                        ErrorCode::ExtensionFailed,
                        in_callback_message(&call),
                    );
                    return;
                }
                let outcome =
                    support::lock(&self.shared.registry).register(id.clone(), &call, &args, false);
                match outcome {
                    RegisterOutcome::Accepted => {
                        self.install_handlers(&id, &call, &args);
                        self.answer_accepted(&id, None);
                    }
                    RegisterOutcome::Rejected { code, message } => {
                        self.answer_rejected(&id, code, message);
                    }
                    RegisterOutcome::FailedPackage { code, message } => {
                        self.answer_rejected(&id, code, message);
                    }
                }
            }
            Ok(calls::Route::Drive { command, args }) => {
                self.drive_command(id, command, args);
            }
            Ok(calls::Route::Host { call: name, args }) => {
                self.host_call(id, &name, args, &context);
            }
            Err((code, message)) => {
                self.answer_rejected(&id, code, message);
            }
        }
    }

    /// Answers one host or state `call` through the injected services.
    /// The entry script's refusals land before decoding, as the Lua half
    /// refuses before it yields. A decoded service operation runs on a
    /// worker, so a slow service never holds up the pump; an ask parks
    /// on its request, then declines; a refresh refuses, since Task 3
    /// owns the holder continuation and no credential exists yet. A
    /// refusal carries the Lua sentence back, as registrations do.
    fn host_call(
        &self,
        id: contract::CommandId,
        call: &str,
        args: Vec<Value>,
        callback: &crate::host::admission::CallbackContext,
    ) {
        use crate::host::admission::CallbackKind;
        let refusal = if matches!(callback.kind, CallbackKind::Entry) {
            crate::host::admission::entry_refusal(call)
        } else {
            None
        };
        if let Some((code, message)) = refusal {
            self.answer_rejected(&id, code, message);
            return;
        }
        // Outside a session only the Call-case set runs: a state write,
        // a host.fs write or host.config rejects here, naming the call,
        // before decoding touches it.
        if support::lock(&self.shared.context).is_call() && !calls::sessionless_allowed(call) {
            let (code, message) = calls::not_available(call);
            self.answer_rejected(&id, code, message);
            return;
        }
        let decoded = match calls::decode_host(call, &args) {
            Ok(decoded) => decoded,
            Err((code, message)) => {
                self.answer_rejected(&id, code, message);
                return;
            }
        };
        match decoded {
            calls::DecodedCall::Service(operation) => {
                self.serve_host(id, call.to_owned(), operation, callback.clone());
            }
            calls::DecodedCall::Ask(_) => {
                self.serve_ask(id, callback.clone());
            }
            calls::DecodedCall::Refresh => {
                self.serve_refresh(id, callback.clone());
            }
        }
    }

    /// Runs one host-call worker on its own thread: every host call
    /// parks on the pump thread's behalf, so the pump never blocks on
    /// the host. A worker that never starts answers `IoFailed`, as a
    /// call the worker never ran.
    fn spawn_host_task(
        &self,
        id: &contract::CommandId,
        task: &str,
        cant_start: String,
        run: Box<dyn FnOnce() + Send + 'static>,
    ) {
        let spawned = std::thread::Builder::new()
            .name(format!("process {task} {}", self.shared.owner))
            .spawn(run);
        if spawned.is_err() {
            self.answer_rejected(id, ErrorCode::IoFailed, cant_start);
        }
    }

    /// Runs one decoded host operation on a worker, answering accepted
    /// with its value or rejected with its code and sentence.
    fn serve_host(
        &self,
        id: contract::CommandId,
        call: String,
        operation: context::HostOperation,
        callback: crate::host::admission::CallbackContext,
    ) {
        let shared = Arc::clone(&self.shared);
        // The context clones before the worker starts: no service call
        // holds the owner lock while it runs.
        let context = support::lock(&shared.context).clone();
        let answer_id = id.clone();
        self.spawn_host_task(
            &id,
            "host",
            format!("{call}: Fiber could not start the host call"),
            Box::new(move || {
                let id = answer_id;
                let cancel = callback.cancel.clone();
                match shared
                    .services
                    .perform(&context, &callback, operation, &cancel)
                {
                    Ok(value) => answer_on(
                        &shared,
                        &id,
                        value.map(|value| {
                            contract::events::CommandResult::Call(contract::events::CallResult {
                                value: Some(value),
                            })
                        }),
                    ),
                    Err((code, message)) => {
                        answer_off(&shared, &id, contract::inbox::Rejection { code, message })
                    }
                }
            }),
        );
    }

    /// Answers one `host.oauth.refresh`: without a credential scope the
    /// callback holds no provider credential, refused with the shared
    /// policy's sentence, as in Lua; with one the native continuation
    /// holds the lock or slot around its nested function request on a
    /// worker, so a slow refresh never holds up the pump.
    fn serve_refresh(
        &self,
        id: contract::CommandId,
        callback: crate::host::admission::CallbackContext,
    ) {
        let Some(scope) = callback.credential.clone() else {
            // No dispatcher set what this refresh refreshes: the refusal
            // names the callback, as the Lua lock arm does.
            self.answer_rejected(
                &id,
                ErrorCode::InvalidArguments,
                crate::host::admission::lock_arg_error("host.oauth.refresh", callback.ran_by()),
            );
            return;
        };
        let shared = Arc::clone(&self.shared);
        let answer_id = id.clone();
        self.spawn_host_task(
            &id,
            "refresh",
            "host.oauth.refresh: Fiber could not start the host call".to_owned(),
            Box::new(move || {
                let id = answer_id;
                oauth::refresh_continue(&shared, &id, callback, scope, &shared.oauth);
            }),
        );
    }

    /// Answers one `host.ask`: at once when no request is answered, and
    /// parked on the request's end otherwise, declining either way, as
    /// the Lua half declines with nobody to answer. The park runs on a
    /// worker, never on the pump thread.
    fn serve_ask(
        &self,
        id: contract::CommandId,
        callback: crate::host::admission::CallbackContext,
    ) {
        let declined = || {
            let value = serde_json::json!({"declined": true});
            contract::events::CommandResult::Call(contract::events::CallResult {
                value: Some(value),
            })
        };
        let Some(request) = callback.request_id().cloned() else {
            answer_on(&self.shared, &id, Some(declined()));
            return;
        };
        let shared = Arc::clone(&self.shared);
        let answer_id = id.clone();
        self.spawn_host_task(
            &id,
            "ask",
            "host.ask: Fiber could not start the host call".to_owned(),
            Box::new(move || {
                let id = answer_id;
                shared.table.wait_end(&request, shared.clock.as_ref());
                answer_on(&shared, &id, Some(declined()));
            }),
        );
    }

    /// The context one `call` with no `request_id` runs under: the entry
    /// script before `ready`, a timer after it, with no timeout of its
    /// own.
    fn call_context(&self) -> crate::host::admission::CallbackContext {
        if self.is_ready() {
            crate::host::admission::CallbackContext::timer(&self.shared.owner)
        } else {
            crate::host::admission::CallbackContext::entry(&self.shared.owner)
        }
    }

    /// Installs one handler per function an accepted registration
    /// registers: every `true` the wire carries becomes a request Fiber
    /// sends. Skips a key the registry does not show, so a dispatch on it
    /// fails closed instead of running a function nothing registered.
    fn install_handlers(&self, command: &contract::CommandId, call: &str, args: &[Value]) {
        for key in calls::handler_keys(call, args) {
            let Some((kind, ran_by, timeout)) = self.handler_spec(command, call, &key, args) else {
                continue;
            };
            let invoke = self.invoke_for(command.clone(), key.clone(), kind, ran_by, timeout);
            support::lock(&self.shared.registry).set_handler(command, &key, invoke);
        }
    }

    /// What one installed key runs as: the registration kind and name in
    /// the Lua callback's words. One site shares the formatting across
    /// every registration kind the spec below matches.
    fn ran_by(kind: &str, name: &str) -> String {
        format!("{kind} {name}")
    }

    /// What one installed key dispatches as: its kind, what the callback
    /// runs as, and the timeout it counts against. `None` when the
    /// registry does not show the key, so the handler never installs.
    fn handler_spec(
        &self,
        command: &contract::CommandId,
        call: &str,
        key: &str,
        args: &[Value],
    ) -> Option<(requests::RequestKind, String, Duration)> {
        let registry = support::lock(&self.shared.registry);
        let name = args.first().and_then(Value::as_str).unwrap_or_default();
        match call {
            "fiber.tool" => {
                let tool = registry.tools().get(name)?;
                let timeout = tool.declared.timeout;
                if key == "run" {
                    Some((
                        requests::RequestKind::Tool,
                        Self::ran_by("tool", name),
                        timeout,
                    ))
                } else {
                    Some((
                        requests::RequestKind::Function,
                        format!("tool {name} effects"),
                        timeout,
                    ))
                }
            }
            "fiber.hook" => {
                let hooks = registry.hooks().by_point.get(name)?;
                let timeout = hooks.last()?.timeout;
                Some((
                    requests::RequestKind::Stream,
                    Self::ran_by("hook", name),
                    timeout,
                ))
            }
            "fiber.watch" => {
                let watchers = &registry.watchers().watchers;
                let timeout = watchers.last()?.timeout;
                let index = registry.watcher_runs().len().saturating_sub(1);
                let _ = command;
                Some((
                    requests::RequestKind::Stream,
                    format!("watcher {index}"),
                    timeout,
                ))
            }
            "fiber.command" => {
                let timeout = registry.commands().get(name)?.timeout;
                Some((
                    requests::RequestKind::Stream,
                    Self::ran_by("command", name),
                    timeout,
                ))
            }
            "fiber.search_backend" => {
                let (timeout, _) = registry.search().get(name)?;
                Some((
                    requests::RequestKind::Function,
                    Self::ran_by("search", name),
                    *timeout,
                ))
            }
            "fiber.provider" => {
                let provider = registry.providers().get(name)?;
                let timeout = provider.timeouts.get(key)?.to_owned();
                Some((
                    requests::RequestKind::Function,
                    format!("provider {name}.{key}"),
                    timeout,
                ))
            }
            "fiber.harness" => {
                let harness = registry.harness().get(name)?;
                Some((
                    requests::RequestKind::Function,
                    format!("harness {name}.{key}"),
                    harness.timeout,
                ))
            }
            _ => None,
        }
    }

    /// The invoker the registry resolves one installed key through: sends
    /// the request and waits for its answer, as `run_request` does. The
    /// invocation's scope rides into the dispatch, so only the
    /// dispatcher that runs a provider's `credential()` refreshes.
    fn invoke_for(
        &self,
        registration: contract::CommandId,
        key: String,
        kind: requests::RequestKind,
        ran_by: String,
        timeout: Duration,
    ) -> crate::runtime::ProcessInvoke {
        let shared = Arc::clone(&self.shared);
        Arc::new(
            move |args: Value,
                  scope: Option<crate::lua::CredentialFor>,
                  caller: &dyn contract::tool::Cancel| {
                requests::run_request(
                    &shared,
                    requests::Dispatch {
                        kind,
                        registration: registration.clone(),
                        key: key.clone(),
                        ran_by: ran_by.clone(),
                        args: requests::wire_args(args),
                        timeout,
                        state_call: None,
                        credential: scope,
                    },
                    caller,
                )
            },
        )
    }
}

/// The sentence a command this Fiber does not build is rejected with,
/// shared with the doors' `not_built`: it names the line's own command.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
fn not_built(command: &str) -> String {
    format!("`{command}` is not built in this Fiber yet.")
}
