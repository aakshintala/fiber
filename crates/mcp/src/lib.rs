//! The MCP client (`docs/mcp.md`): stdio servers Fiber starts as child
//! processes, their tools declared through the tool seam, and their calls
//! run on the server. Reached only through the tool seam
//! (`docs/architecture.md`, "The call rules"): this crate depends on
//! `contract` alone.

mod effects;
mod name;
mod rpc;
mod server;
mod start;
mod tool;

pub use effects::Hints;
pub use server::kill_every_server;
pub use start::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, Servers, Started, start,
};
