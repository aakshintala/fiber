//! A new id from random bytes, such as a session's or a command's
//! (`docs/events.md`, "Identity and ordering").

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

/// A new id from random bytes, such as a session's or a command's
/// (`docs/events.md`, "Identity and ordering"). `RandomState` seeds its keys
/// from the operating system's randomness.
pub fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}

#[cfg(test)]
#[path = "id_tests.rs"]
mod tests;
