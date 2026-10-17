//! Routes connection answers before they enter the loop's bounded queue.

use std::collections::HashMap;
use std::fmt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use contract::HubLine;
use serde_json::{Value, json};

use crate::Input;
use crate::link::{self, Line};

type Map = Box<dyn FnOnce(&HubLine) -> Option<Input> + Send>;

enum Answer {
    Wait(Sender<HubLine>),
    Map(Map),
}

#[derive(Default)]
struct State {
    stream: Option<UnixStream>,
    epoch: u64,
    global_file: Option<std::path::PathBuf>,
    requests: HashMap<String, Answer>,
    feed: Option<(Sender<Input>, Arc<link::Credits>)>,
    #[cfg(test)]
    waiting: usize,
}

/// A connection loss, cancellation, or request bound to an older connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskError {
    Detached,
    Lost,
    Cancelled,
    OtherConnection,
}

impl fmt::Display for AskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Detached | Self::Lost => "connection lost",
            Self::Cancelled => "cancelled",
            Self::OtherConnection => "connection changed",
        })
    }
}

/// Cancellation of a worker waiting for a connection.
#[derive(Clone, Default)]
pub(crate) struct Cancel(Arc<AtomicBool>);
impl Cancel {
    pub(crate) fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub(crate) fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Which thread waits: only the loop lends queue capacity.
pub(crate) enum Caller<'a> {
    Loop,
    Worker(&'a Cancel),
}

/// A registered answer, received without any router or login lock held.
pub(crate) struct Pending {
    epoch: u64,
    rx: Receiver<HubLine>,
}
impl Pending {
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }
    pub(crate) fn wait(self) -> Result<HubLine, AskError> {
        self.rx.recv().map_err(|_| AskError::Lost)
    }
}

/// The terminal's single hub connection and its outstanding commands.
pub(crate) struct Router {
    state: Mutex<State>,
    changed: Condvar,
    #[cfg(test)]
    pause_before_send: Mutex<Option<(Sender<()>, Receiver<()>)>>,
}
impl Router {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            #[cfg(test)]
            pause_before_send: Mutex::new(None),
        })
    }
    pub(crate) fn attach(&self, stream: UnixStream) {
        let mut state = support::lock(&self.state);
        state.epoch += 1;
        state.stream = Some(stream);
        state.global_file = None;
        self.changed.notify_all();
    }
    pub(crate) fn detach(&self) {
        let requests = {
            let mut state = support::lock(&self.state);
            state.stream = None;
            state.global_file = None;
            std::mem::take(&mut state.requests)
        };
        drop(requests);
    }
    pub(super) fn cached_global(&self) -> Option<std::path::PathBuf> {
        support::lock(&self.state).global_file.clone()
    }
    pub(super) fn cache_global(&self, epoch: u64, path: std::path::PathBuf) {
        let mut state = support::lock(&self.state);
        if state.epoch == epoch && state.stream.is_some() {
            state.global_file = Some(path);
        }
    }
    pub(crate) fn wake(&self) {
        let _state = support::lock(&self.state);
        self.changed.notify_all();
    }
    pub(crate) fn set_feed(&self, tx: Sender<Input>, credits: Arc<link::Credits>) {
        support::lock(&self.state).feed = Some((tx, credits));
    }
    pub(crate) fn lend(&self) -> Option<link::Lend> {
        let credits = support::lock(&self.state)
            .feed
            .as_ref()
            .map(|(_, credits)| Arc::clone(credits));
        credits.map(|credits| credits.lend())
    }
    fn write(
        &self,
        epoch: Option<u64>,
        command: &str,
        args: Value,
        answer: Answer,
    ) -> Result<u64, AskError> {
        let (id, stream, epoch) = {
            let mut state = support::lock(&self.state);
            let stream = state.stream.as_ref().ok_or(AskError::Detached)?;
            if epoch.is_some_and(|epoch| epoch != state.epoch) {
                return Err(AskError::OtherConnection);
            }
            let stream = stream.try_clone().map_err(|_| AskError::Lost)?;
            let id = crate::app::mint();
            state.requests.insert(id.clone(), answer);
            (id, stream, state.epoch)
        };
        let mut line = serde_json::Map::from_iter([
            ("id".into(), json!(id)),
            ("command".into(), json!(command)),
        ]);
        if args != json!({}) {
            drop(line.insert("args".into(), args));
        }
        if link::write_line(&stream, &Value::Object(line).to_string()).is_err() {
            support::lock(&self.state).requests.remove(&id);
            return Err(AskError::Lost);
        }
        Ok(epoch)
    }
    pub(crate) fn send(&self, command: &str, args: Value) -> Result<Pending, AskError> {
        let (tx, rx) = mpsc::channel();
        let epoch = self.write(None, command, args, Answer::Wait(tx))?;
        Ok(Pending { epoch, rx })
    }
    pub(crate) fn send_on(
        &self,
        epoch: u64,
        command: &str,
        args: Value,
    ) -> Result<Pending, AskError> {
        let (tx, rx) = mpsc::channel();
        let epoch = self.write(Some(epoch), command, args, Answer::Wait(tx))?;
        Ok(Pending { epoch, rx })
    }
    pub(crate) fn wait_link(&self, cancel: &Cancel) -> Result<(), AskError> {
        let mut state = support::lock(&self.state);
        loop {
            if cancel.is_set() {
                return Err(AskError::Cancelled);
            }
            if state.stream.is_some() {
                return Ok(());
            }
            #[cfg(test)]
            {
                state.waiting += 1;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
            #[cfg(test)]
            {
                state.waiting -= 1;
            }
        }
    }
    pub(crate) fn ask(
        &self,
        command: &str,
        args: Value,
        caller: Caller<'_>,
    ) -> Result<HubLine, AskError> {
        match caller {
            Caller::Loop => {
                let _lend = self.lend();
                self.send(command, args)?.wait()
            }
            Caller::Worker(cancel) => loop {
                if cancel.is_set() {
                    return Err(AskError::Cancelled);
                }
                match self.send(command, args.clone()) {
                    Err(AskError::Detached) => {
                        self.wait_link(cancel)?;
                        #[cfg(test)]
                        if let Some((arrived, release)) =
                            support::lock(&self.pause_before_send).take()
                        {
                            arrived.send(()).unwrap_or(());
                            release.recv().unwrap_or(());
                        }
                    }
                    result => return result?.wait(),
                }
            },
        }
    }
    pub(crate) fn ask_into(&self, command: &str, args: Value, map: Map) -> Result<(), AskError> {
        self.write(None, command, args, Answer::Map(map))
            .map(|_| ())
    }
    pub(crate) fn route(&self, line: &Line) -> bool {
        let Line::Hub(line) = line else {
            return false;
        };
        if !matches!(line.kind.as_str(), "command_accepted" | "command_rejected") {
            return false;
        }
        let Some(id) = line.payload.get("command_id").and_then(Value::as_str) else {
            return false;
        };
        let (answer, feed) = {
            let mut state = support::lock(&self.state);
            (
                state.requests.remove(id),
                state.feed.as_ref().map(|(tx, _)| tx.clone()),
            )
        };
        match answer {
            Some(Answer::Wait(tx)) => {
                tx.send(line.clone()).unwrap_or(());
            }
            Some(Answer::Map(map)) => {
                if let (Some(input), Some(tx)) = (map(line), feed) {
                    tx.send(input).unwrap_or(());
                }
            }
            None => return false,
        }
        true
    }
    #[cfg(test)]
    pub(crate) fn registered(&self) -> usize {
        support::lock(&self.state).requests.len()
    }
    #[cfg(test)]
    pub(crate) fn link_waiters(&self) -> usize {
        support::lock(&self.state).waiting
    }
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
