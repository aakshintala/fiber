//! A Fiber delegate: spawning the child session, watching it to its end,
//! and folding how it ended (`docs/delegates.md`).

pub(crate) mod group;
pub(crate) mod outcome;
// The runner's only caller is the delegate tool (task 3.4); until it
// lands the module is test-only, so the build carries no dead code.
#[allow(dead_code, reason = "the delegate tool drives it in task 3.4")]
pub(crate) mod run;
