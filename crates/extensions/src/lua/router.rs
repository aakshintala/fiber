//! Where an extension's deliveries, ephemeral events and drives go
//! (`docs/extensions.md`, "How an extension runs"): the loop's inbox once
//! `deliver_to` hands it over, or a bounded buffer kept for the first
//! sender; the ephemeral emitter once set, or a bounded buffer flushed on
//! the first one; and the in-process driver. Anything routed after the
//! extension's drop or `seal` is dropped instead, under the same lock that
//! routes deliveries.

use std::collections::VecDeque;
use std::sync::Arc;

use contract::inbox::Delivery;

use super::hub::Hub;
#[cfg(test)]
use super::hub::Window;

/// How many deliveries ending before any inbox are kept, newest first:
/// an extension that ends more runs before any sender still delivers its
/// latest lines in call order.
pub(super) const MAX_PENDING_DELIVERIES: usize = 1024;

/// How many ephemeral events emitted before any emitter are kept, newest
/// first, flushed in call order on the first emitter.
pub(super) const MAX_PENDING_EVENTS: usize = 1024;

/// Where the extension's deliveries, ephemeral events and drives go: the
/// loop's inbox once it arrives, the ephemeral emitter once it arrives,
/// and the in-process driver. Every field is routed under the hub lock.
#[derive(Default)]
pub(super) struct Router {
    /// The loop's inbox for the extension's deliveries, set by `deliver_to`.
    inbox: Option<std::sync::mpsc::Sender<Delivery>>,
    /// Deliveries that ended before any inbox, in call order, sent on the
    /// first one: at most [`MAX_PENDING_DELIVERIES`], oldest dropped first.
    buffer: VecDeque<Delivery>,
    /// The extension was dropped: anything routed after this is dropped,
    /// under the same lock that routes deliveries.
    disposed: bool,
    /// `seal` was called: every later emission and delivery is dropped,
    /// under the same lock that routes them.
    sealed: bool,
    /// The ephemeral emitter `host.status`, `host.widget` and `host.emit`
    /// write through, set late by `emit_to`.
    emitter: Option<Arc<dyn contract::emit::Emit>>,
    /// The in-process driver `host.drive` sends through, set late by
    /// `drive_to`. None before it, or once the extension is sealed.
    driver: Option<Arc<dyn contract::extension::Drive>>,
    /// The session's model `host.model` calls through, set late by
    /// `model_to`. None before it, or once the extension is sealed.
    model: Option<Arc<dyn contract::model::HostModel>>,
    /// The session's delegate starter `host.delegate` starts through, set
    /// late by `delegate_to`. None before it, or once the extension is sealed.
    pub(super) delegate: Option<Arc<dyn contract::extension::HostDelegate>>,
    /// Ephemeral events emitted before any emitter, in call order, flushed
    /// on the first one: at most [`MAX_PENDING_EVENTS`], oldest dropped
    /// first.
    emit_buffer: VecDeque<contract::events::Event>,
}

impl Router {
    /// Routes an extension delivery to the loop's inbox, or buffers it when
    /// no inbox arrived yet. Returns what could not be sent: after `seal`
    /// or the drop, or the inbox's `SendError` on a disconnected receiver.
    /// The caller drops it only after the hub lock is released: a dropped
    /// `Resolved` answers its driver, which may re-enter this hub.
    pub(super) fn route(&mut self, delivery: Delivery) -> Option<Delivery> {
        if self.disposed || self.sealed {
            return Some(delivery);
        }
        match self.inbox.clone() {
            Some(inbox) => match inbox.send(delivery) {
                Ok(()) => None,
                Err(failed) => Some(failed.0),
            },
            None => {
                self.buffer.push_back(delivery);
                // At most the cap is held: the evicted delivery returns
                // unsent, for the caller to drop after the hub lock is
                // released.
                if self.buffer.len() > MAX_PENDING_DELIVERIES {
                    self.buffer.pop_front()
                } else {
                    None
                }
            }
        }
    }

    /// Whether the extension is gone or silenced: dropped or sealed. Each
    /// check for one flag alone stays as it is; only the joint check moves
    /// here.
    pub(super) fn closed(&self) -> bool {
        self.disposed || self.sealed
    }

    /// Records the extension's drop: anything routed after this is dropped.
    pub(super) fn dispose(&mut self) {
        self.disposed = true;
        self.model = None;
        self.delegate = None;
    }

    /// Whether `deliver_to` has given the hub an inbox: a `host.delegate`
    /// before the session runs fails closed instead of reaching the service.
    pub(super) fn has_inbox(&self) -> bool {
        self.inbox.is_some()
    }

    /// The deliveries that ended before any inbox, in call order.
    #[cfg(test)]
    pub(super) fn buffer(&self) -> &VecDeque<Delivery> {
        &self.buffer
    }
}

impl Hub {
    /// Hands the session loop's inbox to the extension's deliveries: one
    /// that ended before any sender is buffered and sent on the first
    /// sender; a later sender replaces the last. A delivery routed after
    /// the extension is dropped is dropped, as is a sender that arrives
    /// after it. Routing holds one lock, so a delivery ending against
    /// `deliver_to` is sent, never stranded: an `mpsc` send never blocks,
    /// so sending under the lock is safe.
    pub(crate) fn set_inbox(&self, inbox: std::sync::mpsc::Sender<Delivery>) {
        // Whatever is not sent is moved out and dropped only after the
        // lock is released: a dropped `Resolved` answers its driver,
        // which may re-enter this hub.
        let unsent: VecDeque<Delivery> = {
            let mut shared = self.lock();
            if shared.router.disposed {
                return;
            }
            if shared.router.sealed {
                // set_inbox_on_a_sealed_hub_sends_nothing: the buffer is
                // moved out, never flushed, so no extension line follows
                // `fiber_exited`.
                std::mem::take(&mut shared.router.buffer)
            } else {
                shared.router.inbox = Some(inbox.clone());
                let buffered = std::mem::take(&mut shared.router.buffer);
                #[cfg(test)]
                self.at_window(Window::Flushing);
                let mut unsent = VecDeque::new();
                let mut failed = false;
                for delivery in buffered {
                    // `send` on an `mpsc` Sender never blocks; held under
                    // the hub lock so sender choice, buffering and
                    // flushing serialize.
                    if !failed {
                        match inbox.send(delivery) {
                            Ok(()) => continue,
                            Err(failed_send) => {
                                failed = true;
                                unsent.push_back(failed_send.0);
                                continue;
                            }
                        }
                    }
                    unsent.push_back(delivery);
                }
                unsent
            }
        };
        drop(unsent);
    }

    /// Sets the ephemeral emitter `host.status`, `host.widget` and `host.emit`
    /// write through, flushing what was buffered before it in call order.
    /// A later emitter replaces the last, as `set_inbox` does. Holds the hub
    /// lock through the flush, so `seal` cannot return between the choice
    /// and the last write: `fiber_exited` follows every flushed line. The
    /// emitter is the log, which takes only its own lock and never calls
    /// back into the hub, so the order hub-then-log never reverses.
    pub(crate) fn set_emit(&self, emit: std::sync::Arc<dyn contract::emit::Emit>) {
        let mut shared = self.lock();
        if shared.router.disposed || shared.router.sealed {
            return;
        }
        shared.router.emitter = Some(emit.clone());
        let buffered = std::mem::take(&mut shared.router.emit_buffer);
        #[cfg(test)]
        self.at_window(Window::EmitFlushing);
        for event in buffered {
            // Ephemeral only, so the log takes its own lock and fans out
            // without file I/O and never calls back into the hub; held
            // under the hub lock so a concurrent `seal` cannot interleave.
            emit.emit(&event);
        }
    }

    /// Emits an ephemeral `event` through the late-bound emitter, or buffers
    /// it in call order when none arrived yet. After the drop or `seal`,
    /// it is dropped. Holds the hub lock through the emission, so `seal`
    /// under the same lock drops every later emission and `fiber_exited`
    /// follows every emitted line. The emitter is the log, which takes only
    /// its own lock and never calls back into the hub, so the order
    /// hub-then-log never reverses.
    pub(crate) fn emit(&self, event: contract::events::Event) {
        let mut shared = self.lock();
        if shared.router.disposed || shared.router.sealed {
            return;
        }
        let Some(emitter) = shared.router.emitter.clone() else {
            shared.router.emit_buffer.push_back(event);
            if shared.router.emit_buffer.len() > MAX_PENDING_EVENTS {
                drop(shared.router.emit_buffer.pop_front());
            }
            return;
        };
        #[cfg(test)]
        self.at_window(Window::Emitting);
        // Held under the hub lock so a concurrent `seal` cannot slip in
        // between the choice and the write; `shared` drops after.
        emitter.emit(&event);
    }

    /// Hands the in-process driver to this extension's `host.drive`.
    /// A later driver replaces the last, as `set_inbox` does.
    pub(crate) fn set_driver(&self, drive: std::sync::Arc<dyn contract::extension::Drive>) {
        let mut shared = self.lock();
        if shared.router.disposed {
            return;
        }
        shared.router.driver = Some(drive);
    }

    /// The driver `host.drive` sends through: none before `drive_to`, once
    /// dropped, or once sealed, so no drive follows `fiber_exited`.
    pub(crate) fn driver(&self) -> Option<std::sync::Arc<dyn contract::extension::Drive>> {
        let shared = self.lock();
        if shared.router.disposed || shared.router.sealed {
            return None;
        }
        shared.router.driver.clone()
    }

    /// Hands the session's model to this extension's `host.model`. A later
    /// model replaces the last, as `set_inbox` does.
    pub(crate) fn set_model(&self, model: std::sync::Arc<dyn contract::model::HostModel>) {
        let mut shared = self.lock();
        if shared.router.disposed {
            return;
        }
        shared.router.model = Some(model);
    }

    /// The model `host.model` calls through: none before `model_to`, once
    /// dropped, or once sealed, so no call follows `fiber_exited`.
    pub(crate) fn model(&self) -> Option<std::sync::Arc<dyn contract::model::HostModel>> {
        let shared = self.lock();
        if shared.router.disposed || shared.router.sealed {
            return None;
        }
        shared.router.model.clone()
    }

    /// Hands the session's delegate starter to this extension's
    /// `host.delegate`. A later service replaces the last, as `set_inbox`
    /// does.
    pub(crate) fn set_delegate(
        &self,
        delegate: std::sync::Arc<dyn contract::extension::HostDelegate>,
    ) {
        let mut shared = self.lock();
        if shared.router.disposed {
            return;
        }
        shared.router.delegate = Some(delegate);
    }

    /// The delegate starter `host.delegate` starts through: none before
    /// `delegate_to`, once dropped, or once sealed, so no start follows
    /// `fiber_exited`.
    pub(crate) fn delegate(&self) -> Option<std::sync::Arc<dyn contract::extension::HostDelegate>> {
        let shared = self.lock();
        if shared.router.disposed || shared.router.sealed {
            return None;
        }
        shared.router.delegate.clone()
    }

    /// Drops every later emission and delivery from this extension; called by
    /// `Session::quiesce` before `fiber_exited`. A running callback is not
    /// stopped; only its output is dropped. The injected model goes with
    /// the seal, and every in-flight call's cancel is set, so no call
    /// follows `fiber_exited`.
    pub(crate) fn seal(&self) {
        let mut shared = self.lock();
        shared.router.sealed = true;
        shared.router.model = None;
        shared.router.delegate = None;
        let cancels: Vec<std::sync::Arc<super::host_calls::ModelCancel>> =
            shared.models.values().cloned().collect();
        drop(shared);
        for cancel in cancels {
            cancel.end();
        }
    }

    /// Routes an extension delivery to the loop's inbox, or buffers it in
    /// call order when no sender arrived yet. One routed after the drop
    /// or after `seal` is dropped instead; routing holds one lock, so nothing is stranded.
    pub(crate) fn send(&self, delivery: Delivery) {
        let unsent = {
            let mut shared = self.lock();
            #[cfg(test)]
            self.at_window(Window::Selected);
            shared.router.route(delivery)
        };
        // Dropped only after the lock is released: a dropped `Resolved`
        // answers its driver, which may re-enter this hub.
        drop(unsent);
    }
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
