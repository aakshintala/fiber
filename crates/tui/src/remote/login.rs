//! A device login bound to the connection that started it.

use super::decode;
use super::{Cancel, Router};
use crate::configure::{BrowserLogin, ConfigureError, LoginShow, Stored};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[cfg(test)]
type Pause = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);

enum LoginState {
    Idle,
    Active { epoch: u64 },
    Cancelled,
}

/// A login whose writes serialize with cancellation, never its answer waits.
pub(crate) struct RemoteLogin {
    router: Arc<Router>,
    name: String,
    shown: Arc<dyn LoginShow>,
    cancel: Cancel,
    state: Mutex<LoginState>,
    #[cfg(test)]
    pause_before_start: Mutex<Option<Pause>>,
    #[cfg(test)]
    pause_before_result: Mutex<Option<Pause>>,
    #[cfg(test)]
    pause_before_result_send: Mutex<Option<Pause>>,
}
impl RemoteLogin {
    pub(super) fn new(router: Arc<Router>, name: String, shown: Arc<dyn LoginShow>) -> Self {
        Self {
            router,
            name,
            shown,
            cancel: Cancel::default(),
            state: Mutex::new(LoginState::Idle),
            #[cfg(test)]
            pause_before_start: Mutex::new(None),
            #[cfg(test)]
            pause_before_result: Mutex::new(None),
            #[cfg(test)]
            pause_before_result_send: Mutex::new(None),
        }
    }
    #[cfg(test)]
    pub(crate) fn l1_free(&self) -> bool {
        self.state.try_lock().is_ok()
    }
}
#[cfg(test)]
fn pause(point: &Mutex<Option<Pause>>) {
    let point = support::lock(point).take();
    if let Some((arrived, release)) = point {
        arrived.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }
}
impl BrowserLogin for RemoteLogin {
    fn run(&self) -> Result<Stored, ConfigureError> {
        let pending = loop {
            let mut state = support::lock(&self.state);
            if matches!(*state, LoginState::Cancelled) {
                return Err(super::ask_error(super::AskError::Cancelled));
            }
            #[cfg(test)]
            pause(&self.pause_before_start);
            match self.router.send("device_login", json!({"name":self.name})) {
                Ok(pending) => {
                    *state = LoginState::Active {
                        epoch: pending.epoch(),
                    };
                    break pending;
                }
                Err(super::AskError::Detached) => {
                    drop(state);
                    self.router
                        .wait_link(&self.cancel)
                        .map_err(super::ask_error)?;
                }
                Err(error) => return Err(super::ask_error(error)),
            }
        };
        let epoch = pending.epoch();
        let line = pending.wait().map_err(super::ask_error)?;
        let answer = decode::result(&line, "device_login")?;
        let url = answer
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| decode::io_error("device_login"))?;
        let code = answer
            .get("code")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| decode::io_error("device_login"))?;
        self.shown.show(url, code);
        #[cfg(test)]
        pause(&self.pause_before_result);
        let pending = {
            let state = support::lock(&self.state);
            if matches!(*state, LoginState::Cancelled) {
                return Err(super::ask_error(super::AskError::Cancelled));
            }
            #[cfg(test)]
            pause(&self.pause_before_result_send);
            self.router
                .send_on(epoch, "device_login_result", json!({}))
                .map_err(super::ask_error)?
        };
        let line = pending.wait().map_err(super::ask_error)?;
        let answer = decode::result(&line, "device_login_result")?;
        let path = answer
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| decode::io_error("device_login_result"))?;
        Ok(Stored {
            path: path.into(),
            replaced: false,
        })
    }
    fn cancel(&self) {
        let pending = {
            let mut state = support::lock(&self.state);
            let previous = std::mem::replace(&mut *state, LoginState::Cancelled);
            self.cancel.set();
            self.router.wake();
            match previous {
                LoginState::Idle | LoginState::Cancelled => return,
                LoginState::Active { epoch } => {
                    self.router.send_on(epoch, "device_login_cancel", json!({}))
                }
            }
        };
        if let Ok(pending) = pending {
            let _lend = self.router.lend();
            drop(pending.wait());
        }
    }
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
