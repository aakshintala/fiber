//! MCP token storage in Fiber home (`docs/state.md`, "Credentials").
use crate::{ConfigError, CredentialFile, CredentialLock};
use contract::mcp_tokens::{HeldTokenFile, TokenFileError, TokenFiles};
use serde_json::Value;
use std::path::PathBuf;

/// Credential files for remote MCP servers, keyed by canonical URL hash.
#[derive(Debug, Clone)]
pub struct McpTokenFiles {
    home: PathBuf,
}
impl McpTokenFiles {
    /// Uses the given Fiber home without creating any directory.
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
}
impl TokenFiles for McpTokenFiles {
    fn read(&self, key: &str) -> Result<Option<Value>, TokenFileError> {
        CredentialFile::new(&self.home, "mcp", key)
            .and_then(|file| file.read_unlocked())
            .map_err(read_error)
    }
    fn try_lock(&self, key: &str) -> Result<Option<Box<dyn HeldTokenFile>>, TokenFileError> {
        CredentialFile::new(&self.home, "mcp", key)
            .and_then(|file| file.try_lock())
            .map_err(read_error)
            .map(|held| held.map(|lock| Box::new(lock) as Box<dyn HeldTokenFile>))
    }
}
impl HeldTokenFile for CredentialLock {
    fn read(&self) -> Result<Option<Value>, TokenFileError> {
        CredentialLock::read(self).map_err(read_error)
    }
    fn write(&self, value: &Value) -> Result<(), TokenFileError> {
        CredentialLock::write(self, value).map_err(|error| {
            let detail = error.to_string();
            match error {
                ConfigError::NotPlain { .. } => TokenFileError::Refused(detail),
                ConfigError::Unsynced { .. } => TokenFileError::Unsynced(detail),
                ConfigError::Io { .. }
                | ConfigError::Json { .. }
                | ConfigError::WrongType { .. }
                | ConfigError::FiberHome(_)
                | ConfigError::Override { .. }
                | ConfigError::Refused { .. }
                | ConfigError::ProjectKey { .. }
                | ConfigError::Shape { .. }
                | ConfigError::CredentialMissing { .. }
                | ConfigError::CredentialFailed { .. }
                | ConfigError::DuplicateExtension { .. }
                | ConfigError::SecretName { .. }
                | ConfigError::ThemeRead(..)
                | ConfigError::BadThemeName => TokenFileError::Unwritable(detail),
            }
        })
    }
}
fn read_error(error: ConfigError) -> TokenFileError {
    let detail = error.to_string();
    match error {
        ConfigError::NotPlain { .. } => TokenFileError::Refused(detail),
        ConfigError::Io { .. }
        | ConfigError::Unsynced { .. }
        | ConfigError::Json { .. }
        | ConfigError::WrongType { .. }
        | ConfigError::FiberHome(_)
        | ConfigError::Override { .. }
        | ConfigError::Refused { .. }
        | ConfigError::ProjectKey { .. }
        | ConfigError::Shape { .. }
        | ConfigError::CredentialMissing { .. }
        | ConfigError::CredentialFailed { .. }
        | ConfigError::DuplicateExtension { .. }
        | ConfigError::SecretName { .. }
        | ConfigError::ThemeRead(..)
        | ConfigError::BadThemeName => TokenFileError::Unreadable(detail),
    }
}
#[cfg(test)]
#[path = "mcp_tokens_tests.rs"]
mod tests;
