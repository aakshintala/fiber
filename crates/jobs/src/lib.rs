//! Background jobs: starting, watching and stopping them
//! (`docs/tools.md`, "Background jobs"). Reached only through the tool seam
//! (`docs/architecture.md`).

mod delegate;
mod error;
mod registry;
#[cfg(test)]
mod support;
#[cfg(test)]
mod timing_tests;
mod tool;

pub use delegate::{
    CreateWorktree, CreatedWorktree, DelegateFork, DelegateMessage, DelegateModels,
    DelegateSessions, DelegateSpawn, DelegateWiring, EndWorktree, FinishWorktree, ForkPoint,
    Forking, Fresh, LastSeq, Launch, Launched, Resolve, Start, Steer, Steered, Watch, Watched,
    WorktreeGuard,
};
pub use error::JobsError;
pub use registry::Registry;
pub use tool::JobsTool;
