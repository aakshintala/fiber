//! Connection-owned device login, with one answer per command and per-login waiters.
use crate::connection::{Hub, Refused, lock};
use crate::feed::{answer, invalid};
use contract::shapes::Failure;
use contract::{CommandId, ErrorCode};
use serde_json::{Map, Value, json};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(test)]
type AfterFinish = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;
type Outcome = Result<String, Failure>;
type Slot = Arc<Mutex<Option<Record>>>;
type Superseded = Arc<Mutex<Option<CommandId>>>;
enum State {
    Running(Arc<dyn crate::DeviceLogin>),
    Done(Outcome),
}
struct Record {
    id: u64,
    state: State,
    waiter: Option<CommandId>,
    superseded: Superseded,
}

#[derive(Default)]
pub(crate) struct Login {
    slot: Slot,
    next: u64,
    #[cfg(test)]
    pub(crate) after_finish: AfterFinish,
}
impl Login {
    pub(crate) fn start(
        &mut self,
        id: &CommandId,
        args: &Map<String, Value>,
        hub: &Arc<Hub>,
        writer: &Arc<Mutex<UnixStream>>,
    ) {
        let result = self.begin(id, args, hub, writer);
        if let Err(error) = result {
            answer(writer, hub, id, Err(error));
        }
    }
    fn begin(
        &mut self,
        id: &CommandId,
        args: &Map<String, Value>,
        hub: &Arc<Hub>,
        writer: &Arc<Mutex<UnixStream>>,
    ) -> Result<(), Refused> {
        if args.len() != 1 {
            return Err(invalid());
        }
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?
            .to_owned();
        let answered = Arc::new(AtomicBool::new(false));
        let shown_answered = answered.clone();
        let w = writer.clone();
        let h = hub.clone();
        let command = id.clone();
        let shown = Box::new(move |url: &str, code: &str| {
            if !shown_answered.swap(true, Ordering::SeqCst) {
                answer(&w, &h, &command, Ok(Some(json!({"url":url,"code":code}))));
            }
        });
        let login =
            hub.machine
                .as_ref()
                .ok_or_else(invalid)?
                .device_login(&name, shown, hub.clock.clone());
        self.next = self.next.wrapping_add(1);
        let generation = self.next;
        let superseded = Arc::new(Mutex::new(None));
        let old = {
            let mut slot = lock(&self.slot);
            let old = slot.take();
            let previous = retire(old);
            *slot = Some(Record {
                id: generation,
                state: State::Running(login.clone()),
                waiter: None,
                superseded: superseded.clone(),
            });
            previous
        };
        if let Some(old) = old {
            old.cancel();
        }
        let slot = self.slot.clone();
        let w = writer.clone();
        let h = hub.clone();
        let command = id.clone();
        #[cfg(test)]
        let after_finish = self.after_finish.clone();
        let spawned = crate::connection::spawn(hub, "hub-login", move || {
            let outcome = login.run();
            let outcome = if !answered.swap(true, Ordering::SeqCst) {
                let error = match outcome {
                    Err(error) => error,
                    Ok(_) => Failure {
                        code: ErrorCode::AuthenticationFailed,
                        message: format!("`{name}` has no device-code form."),
                        provider: None,
                        retry_after_ms: None,
                    },
                };
                let waiter = finish(&slot, generation, &superseded, Err(error.clone()), false);
                answer(&w, &h, &command, Err(refused(&error)));
                waiter
            } else {
                finish(&slot, generation, &superseded, outcome, true)
            };
            if let Some((waiter, outcome)) = outcome {
                answer_result(&w, &h, &waiter, outcome);
            }
            #[cfg(test)]
            let callback = lock(&after_finish).take();
            #[cfg(test)]
            if let Some(callback) = callback {
                callback();
            }
        });
        if let Err(error) = spawned {
            lock(&self.slot).take();
            return Err(Refused {
                code: ErrorCode::IoFailed,
                message: format!("The login could not start: {error}."),
            });
        }
        Ok(())
    }
    pub(crate) fn result(
        &mut self,
        id: &CommandId,
        args: &Map<String, Value>,
        hub: &Hub,
        writer: &Arc<Mutex<UnixStream>>,
    ) {
        let result = if !args.is_empty() {
            Err(invalid())
        } else {
            let mut slot = lock(&self.slot);
            match slot.as_mut() {
                None => Err(stale()),
                Some(record) if record.waiter.is_some() => Err(stale()),
                Some(record) => match &record.state {
                    State::Running(_) => {
                        record.waiter = Some(id.clone());
                        Ok(None)
                    }
                    State::Done(_) => match slot.take().map(|record| record.state) {
                        Some(State::Done(result)) => Ok(Some(result)),
                        _ => Err(stale()),
                    },
                },
            }
        };
        match result {
            Err(error) => answer(writer, hub, id, Err(error)),
            Ok(Some(outcome)) => answer_result(writer, hub, id, outcome),
            Ok(None) => {}
        }
    }
    pub(crate) fn cancel(
        &mut self,
        id: &CommandId,
        args: &Map<String, Value>,
        hub: &Hub,
        writer: &Arc<Mutex<UnixStream>>,
    ) {
        let result = if !args.is_empty() {
            Err(invalid())
        } else {
            let mut slot = lock(&self.slot);
            match slot.as_ref().map(|record| &record.state) {
                None => Err(stale()),
                Some(State::Running(login)) => Ok(Some(login.clone())),
                Some(State::Done(_)) => {
                    slot.take();
                    Ok(None)
                }
            }
        };
        let result = result.map(|login| {
            if let Some(login) = login {
                login.cancel();
            }
            None
        });
        answer(writer, hub, id, result);
    }
}
impl Drop for Login {
    fn drop(&mut self) {
        let login = {
            let mut slot = lock(&self.slot);
            retire(slot.take())
        };
        if let Some(login) = login {
            login.cancel();
        }
    }
}
// The reader moves the waiter while holding Slot; finish checks Slot before
// looking in Superseded. Either order preserves the old login's own waiter.
fn retire(record: Option<Record>) -> Option<Arc<dyn crate::DeviceLogin>> {
    let record = record?;
    match record.state {
        State::Running(login) => {
            *lock(&record.superseded) = record.waiter;
            Some(login)
        }
        State::Done(_) => None,
    }
}
fn finish(
    slot: &Slot,
    id: u64,
    superseded: &Superseded,
    outcome: Outcome,
    shown: bool,
) -> Option<(CommandId, Outcome)> {
    {
        let mut slot = lock(slot);
        if slot.as_ref().map(|record| record.id) == Some(id) {
            let mut record = slot.take()?;
            if let Some(waiter) = record.waiter.take() {
                return Some((waiter, outcome));
            }
            if shown {
                record.state = State::Done(outcome);
                *slot = Some(record);
            }
            return None;
        }
    }
    lock(superseded).take().map(|waiter| (waiter, outcome))
}
fn answer_result(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, id: &CommandId, outcome: Outcome) {
    answer(
        writer,
        hub,
        id,
        outcome
            .map(|path| Some(json!({"path":path})))
            .map_err(|error| refused(&error)),
    );
}
fn refused(error: &Failure) -> Refused {
    Refused {
        code: error.code.clone(),
        message: error.message.clone(),
    }
}
fn stale() -> Refused {
    Refused {
        code: ErrorCode::StaleRequest,
        message: "There is no pending device login for this command.".into(),
    }
}

#[cfg(test)]
#[path = "device_login_tests.rs"]
mod tests;
