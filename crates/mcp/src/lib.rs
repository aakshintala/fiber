//! The MCP client (`docs/mcp.md`): stdio servers Fiber starts as child
//! processes and remote HTTP servers with URL-bound OAuth tokens. Their tools
//! are declared through the tool seam, and their calls run on the server. Reached only through the tool seam
//! (`docs/architecture.md`, "The call rules"): this crate depends on
//! `contract` and `support`.

mod cache;
mod effects;
mod elicit;
mod fail;
mod http;
mod name;
mod pipes;
mod prompt;
mod registry;
mod resources;
mod rpc;
mod server;
mod server_json;
mod slot;
mod sse;
mod start;
#[cfg(test)]
mod test_support;
mod token;
mod tool;
mod wait;

pub use effects::Hints;
pub use prompt::Prompts;
pub use registry::stop_every_start;
pub use resources::{RESOURCES_TOOL, resources_definition};
pub use start::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, Program, ServerSpec, Servers, Started,
    StdioSpec, Transport, start,
};
pub use token::{Token, TokenError, store_token, url_has_userinfo};
