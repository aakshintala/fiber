//! The MCP client (`docs/mcp.md`): stdio servers Fiber starts as child
//! processes and remote HTTP servers with URL-bound OAuth tokens. Their tools
//! are declared through the tool seam, and their calls run on the server. Reached only through the tool seam
//! (`docs/architecture.md`, "The call rules"): this crate depends on
//! `contract` and `support`.

mod cache;
mod effects;
mod elicit;
mod error;
mod fail;
mod http;
mod login;
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
mod sync;
#[cfg(test)]
mod test_support;
#[cfg(test)]
#[path = "timing_tests.rs"]
mod timing_tests;
mod token;
mod tool;
mod wait;

pub use effects::Hints;
pub use login::{CALLBACK_WAIT, LoginError, login};
pub use prompt::Prompts;
pub use registry::stop_every_start;
pub use resources::{RESOURCES_TOOL, resources_definition};
pub use start::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, Program, Restarted, SecretError, SecretReader,
    ServerSpec, Servers, Started, StdioSpec, Transport, restart, start, start_observed,
};
pub use sync::{
    Disposition, StagedSync, SyncInputs, SyncRow, Synced, classify_sync, commit_sync, stage_sync,
};
pub use token::{Token, TokenError, store_token, token_key, url_has_userinfo};
