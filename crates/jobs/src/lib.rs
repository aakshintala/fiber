//! Background jobs: starting, watching and stopping them
//! (`docs/tools.md`, "Background jobs"). Reached only through the tool seam
//! (`docs/architecture.md`).

mod delegate;
mod registry;
mod tool;

pub use delegate::{DelegateSpawn, Launch, Launched, Resolve, Watch, Watched, kill_every_group};
pub use registry::Registry;
pub use tool::JobsTool;
