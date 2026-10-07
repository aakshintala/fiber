//! A secret's value (`docs/configuration.md`, "Secrets"). It lives here, not
//! in `config`, because `provider` carries a key to the line that sends it
//! and depends only on `contract` (`docs/architecture.md`, "The call rules").

use std::fmt;

/// A secret's value. It never prints: `Debug` shows only that it is a secret,
/// and there is no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value, such as one `fiber login` was given.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The value itself, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(redacted)")
    }
}
