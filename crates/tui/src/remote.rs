//! Configuration on the hub (`docs/tui.md`, "Swapped views"). Loop-thread
//! calls hold a credit lend while waiting. Workers wait for a link without
//! lending credits. Login state is locked only across register-and-write,
//! never across an answer or link wait; its epoch binds result and cancel
//! commands to the connection that started the login.
//!
//! Lock order: login state, router state, then the link write lock. Router
//! state is released before writing; credits are never locked under login
//! state or router state. No lock is held while waiting for an answer.

mod router;

pub(crate) use router::{AskError, Caller, Cancel, Pending, Router};
