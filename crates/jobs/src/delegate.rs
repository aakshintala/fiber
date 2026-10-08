//! A Fiber delegate: spawning the child session, watching it to its end,
//! and folding how it ended (`docs/delegates.md`).

pub(crate) mod group;
// The fold's only caller is the runner (task 3.3); until it lands the
// module is test-only, so the build carries no dead code.
#[cfg(test)]
pub(crate) mod outcome;
