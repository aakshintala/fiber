//! The extension thread's inbox (`docs/extensions.md`, "A host call suspends
//! the code that made it"). A callback parked on `host.http` does not hold
//! the thread: the request runs elsewhere, and another callback runs
//! meanwhile. Each parked callback still ends at its own deadline.

use std::io;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Instant;

use mlua::Thread;
use serde_json::Value;

use crate::Error;
use crate::host;

use super::{CallbackKind, Job, Reply, Step, Vm, expired, timeout_ms};

/// A job, or the reply of a `host.http` that was running off this thread.
pub(super) enum Msg {
    Job(Job),
    Http(u64, Result<(u16, Vec<u8>), String>),
}

/// A callback suspended on `host.http`. Its deadline keeps running.
struct Parked {
    id: u64,
    thread: Thread,
    deadline: Option<Instant>,
    timeout: std::time::Duration,
    callback: String,
    kind: CallbackKind,
    reply: Sender<Reply>,
}

/// The extension's thread. A VM whose entry script failed is created again
/// on the next job. It quits as soon as a caller has stopped waiting: that
/// caller abandoned the VM, so nothing may run after it.
/// The extension thread's VM and the callbacks parked on `host.http`.
struct Serve<'a> {
    name: &'a str,
    dir: &'a Path,
    home: &'a Path,
    vm: &'a mut Option<Vm>,
    parked: &'a mut Vec<Parked>,
    next_id: &'a mut u64,
    http: &'a Sender<Msg>,
}

pub(super) fn serve(
    name: &str,
    dir: &Path,
    home: &Path,
    inbox: &Receiver<Msg>,
    http: &Sender<Msg>,
) {
    let mut vm = None;
    let mut parked: Vec<Parked> = Vec::new();
    let mut next_id = 0u64;
    let mut serve = Serve {
        name,
        dir,
        home,
        vm: &mut vm,
        parked: &mut parked,
        next_id: &mut next_id,
        http,
    };
    loop {
        if !release(serve.name, serve.parked) {
            return;
        }
        let msg = if serve.parked.is_empty() {
            match inbox.recv() {
                Ok(msg) => msg,
                Err(std::sync::mpsc::RecvError) => return,
            }
        } else {
            let wait = serve
                .parked
                .iter()
                .filter_map(|p| p.deadline)
                .min()
                .map(|at| at.saturating_duration_since(Instant::now()));
            match wait {
                Some(wait) => match inbox.recv_timeout(wait) {
                    Ok(msg) => msg,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match inbox.recv() {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::RecvError) => return,
                },
            }
        };
        let stay = match msg {
            Msg::Job(job) => serve.start_job(job),
            Msg::Http(id, result) => serve.resume_http(id, result),
        };
        if !stay {
            return;
        }
    }
}

/// Completes every parked callback whose deadline has passed. False when a
/// caller has stopped waiting and the thread must quit.
fn release(name: &str, parked: &mut Vec<Parked>) -> bool {
    while let Some(pos) = parked.iter().position(|p| expired(p.deadline)) {
        let parked = parked.swap_remove(pos);
        if !finish(
            &parked.reply,
            Err(Error::Timeout {
                extension: name.to_owned(),
                callback: parked.callback,
                timeout_ms: timeout_ms(parked.timeout),
            }),
        ) {
            return false;
        }
    }
    true
}

/// Sends `result` unless it says the caller already left, or the send fails.
/// False means the thread must quit.
fn finish(reply: &Sender<Reply>, result: Result<Value, Error>) -> bool {
    if matches!(result, Err(Error::Stopped { .. })) {
        return false;
    }
    reply.send(Reply::Done(result)).is_ok()
}

impl Serve<'_> {
    fn start_job(&mut self, job: Job) -> bool {
        let Job { target, arg, reply } = job;
        let step = {
            let name = self.name;
            let notify = |at| {
                reply.send(Reply::Deadline(at)).map_err(|_| Error::Stopped {
                    extension: name.to_owned(),
                })
            };
            if self.vm.is_none() {
                match Vm::load(self.name, self.dir, self.home, &notify) {
                    Ok(loaded) => *self.vm = Some(loaded),
                    Err(e) => return finish(&reply, Err(e)),
                }
            }
            let Some(vm) = self.vm.as_ref() else {
                return finish(&reply, Err(self.stopped()));
            };
            vm.step(&target, &arg, &notify)
        };
        self.settle(reply, step)
    }

    fn resume_http(&mut self, id: u64, result: Result<(u16, Vec<u8>), String>) -> bool {
        let Some(pos) = self.parked.iter().position(|p| p.id == id) else {
            return true;
        };
        let parked_cb = self.parked.swap_remove(pos);
        if expired(parked_cb.deadline) {
            return finish(
                &parked_cb.reply,
                Err(Error::Timeout {
                    extension: self.name.to_owned(),
                    callback: parked_cb.callback,
                    timeout_ms: timeout_ms(parked_cb.timeout),
                }),
            );
        }
        let (reply, step) = {
            let Some(vm) = self.vm.as_ref() else {
                return true;
            };
            let args = match host::resume_values(&vm.lua, result) {
                Ok(args) => args,
                Err(e) => return finish(&parked_cb.reply, Err(vm.error(&e))),
            };
            let Parked {
                thread,
                deadline,
                timeout,
                callback,
                kind,
                reply,
                ..
            } = parked_cb;
            let step = vm.after(thread, args, &callback, timeout, deadline, kind);
            (reply, step)
        };
        self.settle(reply, step)
    }

    /// Sends a finished callback's result, or parks it on another `host.http`.
    fn settle(&mut self, reply: Sender<Reply>, step: Result<Step, Error>) -> bool {
        match step {
            Ok(Step::Done(value)) => finish(&reply, Ok(value)),
            Ok(Step::Suspend {
                thread,
                deadline,
                timeout,
                callback,
                kind,
                request,
            }) => {
                let id = *self.next_id;
                *self.next_id = self.next_id.wrapping_add(1);
                match spawn_http(self.name, id, request, self.http) {
                    Ok(()) => {
                        self.parked.push(Parked {
                            id,
                            thread,
                            deadline,
                            timeout,
                            callback,
                            kind,
                            reply,
                        });
                        true
                    }
                    Err(source) => finish(
                        &reply,
                        Err(Error::Io {
                            path: self.dir.to_owned(),
                            source,
                        }),
                    ),
                }
            }
            Err(e) => finish(&reply, Err(e)),
        }
    }

    fn stopped(&self) -> Error {
        Error::Stopped {
            extension: self.name.to_owned(),
        }
    }
}

fn spawn_http(name: &str, id: u64, request: host::HttpRequest, tx: &Sender<Msg>) -> io::Result<()> {
    let tx = tx.clone();
    thread::Builder::new()
        .name(format!("http {name}"))
        .spawn(move || {
            let result = host::perform(&request);
            // The extension thread has quit when the send fails.
            drop(tx.send(Msg::Http(id, result)));
        })?;
    Ok(())
}
