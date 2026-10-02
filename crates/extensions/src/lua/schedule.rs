//! The extension thread's inbox (`docs/extensions.md`, "How an extension
//! runs"). A callback parked on `host.http` does not hold the thread: the
//! request runs elsewhere, and provider work runs meanwhile. The next
//! command does not. It stays queued until the parked command finishes.
//! Each parked callback still ends at its own deadline.

use std::cell::Cell;
use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Instant;

use mlua::Thread;
use serde_json::Value;

use crate::Error;
use crate::host;

use super::{CallbackKind, CallbackTimeouts, Job, Reply, Step, Target, Vm, expired, timeout_ms};

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

/// The extension thread's VM, the callbacks parked on `host.http`, and the
/// commands queued behind one.
struct Serve<'a> {
    name: &'a str,
    dir: &'a Path,
    home: &'a Path,
    vm: &'a mut Option<Vm>,
    parked: &'a mut Vec<Parked>,
    queued: &'a mut VecDeque<Job>,
    next_id: &'a mut u64,
    http: &'a Sender<Msg>,
    timeouts: &'a Mutex<CallbackTimeouts>,
}

pub(super) fn serve(
    name: &str,
    dir: &Path,
    home: &Path,
    inbox: &Receiver<Msg>,
    http: &Sender<Msg>,
    timeouts: &Mutex<CallbackTimeouts>,
) {
    let mut vm = None;
    let mut parked: Vec<Parked> = Vec::new();
    let mut queued = VecDeque::new();
    let mut next_id = 0u64;
    let mut serve = Serve {
        name,
        dir,
        home,
        vm: &mut vm,
        parked: &mut parked,
        queued: &mut queued,
        next_id: &mut next_id,
        http,
        timeouts,
    };
    loop {
        if !release(serve.name, serve.parked) || !serve.pump() {
            return;
        }
        let busy = !serve.parked.is_empty();
        let msg = if !busy {
            match inbox.recv() {
                Ok(msg) => msg,
                Err(std::sync::mpsc::RecvError) => return,
            }
        } else {
            match serve.next_wake() {
                Some(at) => {
                    match inbox.recv_timeout(at.saturating_duration_since(Instant::now())) {
                        Ok(msg) => msg,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match inbox.recv() {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::RecvError) => return,
                },
            }
        };
        let stay = match msg {
            Msg::Job(job) => serve.on_job(job),
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

/// Sends `result` after a callback ran. False when the caller has gone: that
/// caller abandoned a VM the hook could not stop, so the thread must quit.
fn finish(reply: &Sender<Reply>, result: Result<Value, Error>) -> bool {
    reply.send(Reply::Done(result)).is_ok()
}

/// Sends `result` for a call that never reached Lua. The caller may already
/// have failed it on its own deadline. Either way the VM stays.
fn note(reply: &Sender<Reply>, result: Result<Value, Error>) {
    drop(reply.send(Reply::Done(result)));
}

impl Serve<'_> {
    /// Starts `job` now, or queues it when an earlier command is still parked.
    fn on_job(&mut self, job: Job) -> bool {
        let command = matches!(job.target, Target::Command(_));
        if command && self.command_parked() {
            self.queued.push_back(job);
            return true;
        }
        self.start_job(job)
    }

    fn command_parked(&self) -> bool {
        self.parked
            .iter()
            .any(|p| matches!(p.kind, CallbackKind::Command))
    }

    /// Starts the next queued command once no command is parked.
    fn pump(&mut self) -> bool {
        if self.command_parked() {
            return true;
        }
        while let Some(job) = self.queued.pop_front() {
            if !self.start_job(job) {
                return false;
            }
            if self.command_parked() {
                return true;
            }
        }
        true
    }

    fn next_wake(&self) -> Option<Instant> {
        self.parked.iter().filter_map(|p| p.deadline).min()
    }

    fn start_job(&mut self, job: Job) -> bool {
        let Job {
            target,
            arg,
            reply,
            asked,
        } = job;
        if self.vm.is_none() {
            let name = self.name;
            let notify = |at| {
                reply.send(Reply::Deadline(at)).map_err(|_| Error::Stopped {
                    extension: name.to_owned(),
                })
            };
            match Vm::load(self.name, self.dir, self.home, &notify, self.timeouts) {
                Ok(loaded) => *self.vm = Some(loaded),
                Err(Error::Stopped { .. }) => return true,
                Err(e) => {
                    note(&reply, Err(e));
                    return true;
                }
            }
        }
        // Set once Lua is about to run. An error before that, including a
        // deadline that passed while this call waited, fails this call alone.
        let announced = Cell::new(false);
        let step = {
            let name = self.name;
            let notify = |at| {
                announced.set(true);
                reply.send(Reply::Deadline(at)).map_err(|_| Error::Stopped {
                    extension: name.to_owned(),
                })
            };
            let Some(vm) = self.vm.as_ref() else {
                note(&reply, Err(self.stopped()));
                return true;
            };
            vm.step(&target, &arg, asked, &notify)
        };
        match step {
            // The caller left before Lua ran. The VM has not been tainted.
            Err(Error::Stopped { .. }) => true,
            Err(e) if !announced.get() => {
                note(&reply, Err(e));
                true
            }
            other => self.settle(reply, other),
        }
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
