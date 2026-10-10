//! The MCP client (`docs/mcp.md`): stdio servers Fiber starts as child
//! processes, their tools declared through the tool seam, and their calls
//! run on the server. Reached only through the tool seam
//! (`docs/architecture.md`, "The call rules"): this crate depends on
//! `contract` alone.

mod cache;
mod effects;
mod fail;
mod name;
mod pipes;
mod prompt;
mod registry;
mod rpc;
mod server;
mod server_json;
mod slot;
mod start;
#[cfg(test)]
mod test_support;
mod tool;
mod wait;

pub use effects::Hints;
pub use prompt::Prompts;
pub use registry::stop_every_start;
pub use start::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, Servers, Started, start,
};
