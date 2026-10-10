//! The process clock (`docs/architecture.md`, "The call rules"): built once
//! by `main` and passed down. The only production caller of the process
//! clock.

pub(crate) use support::clock::System;

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
