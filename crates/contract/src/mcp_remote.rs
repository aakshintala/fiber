//! The MCP seam `cli` speaks through (`docs/architecture.md`, "The call
//! rules"): `cli` never reaches `mcp`, and `main` implements this trait over
//! `mcp` and passes it in.

use std::sync::Arc;

use crate::clock::Clock;
use crate::diag::DebugLog;
use crate::mcp_tokens::TokenFiles;
use crate::shapes::Failure;

/// What `cli`'s MCP commands ask of a server's URL.
pub trait McpRemote {
    /// Whether the URL's authority carries a user name or password.
    fn has_userinfo(&self, url: &str) -> bool;
    /// The file name of the token for `url`: the lowercase hex SHA-256 of
    /// its canonical bytes.
    fn token_key(&self, url: &str) -> Result<String, Failure>;
    /// Logs in to the server at `url`, showing the authorization URL
    /// through `present`. A `Failure` carries the login error's code.
    /// `debug` observes the flow's requests; `None` stays silent.
    fn login(
        &self,
        files: &dyn TokenFiles,
        url: &str,
        present: &dyn Fn(&str),
        clock: &dyn Clock,
        debug: Option<Arc<dyn DebugLog>>,
    ) -> Result<(), Failure>;
}
