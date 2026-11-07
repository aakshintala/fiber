//! Contract-dependent fakes for tests and jigs (`docs/testing/fakes.md`,
//! "Fakes"): scripted providers, recorders and domain stand-ins. Re-exports
//! the contract-free helpers from `fakes` with their original type identity.
//! Product crates use this package only as a test dependency; no release
//! binary contains it (`docs/architecture.md`, "The call rules").

mod blocking;
pub mod emit;
mod handoff;
pub mod jobs;
mod locks;
mod rules;
mod scripted_provider;
mod skills;
mod tool_io;

pub use blocking::BlockingProvider;
pub use emit::{EventLog, Recorder};
pub use fakes::*;
pub use handoff::no_note;
pub use jobs::StubJobs;
pub use locks::{FakeLock, NoLock};
pub use rules::{NoRules, RecordingRules};
pub use scripted_provider::{
    Scripted, ScriptedProvider, call_usage, reply, unnamed_usage, wire_tools,
};
pub use skills::NoSkills;
pub use tool_io::{text_of, tool_args};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
