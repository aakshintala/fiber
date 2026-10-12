//! The MCP seam `cli` speaks through (`docs/architecture.md`, "The call
//! rules"): `cli` never reaches `mcp`, and `main` implements this trait over
//! `mcp` and passes it in.

/// What `cli`'s MCP commands ask of a server's URL.
pub trait McpRemote {
    /// Whether the URL's authority carries a user name or password.
    fn has_userinfo(&self, url: &str) -> bool;
}
