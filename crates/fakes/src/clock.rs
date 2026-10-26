//! A clock whose time moves only when a test says so (`docs/testing.md`,
//! "Fakes" and "Values that change every run"). The implementation lives in
//! [`support::clock`]; this module re-exports it so `fakes::clock::FakeClock`
//! and every `fakes::clock::` path keep resolving.

/// The process clock tests use (`docs/testing.md`, "Values that change
/// every run"): the operating system's clock in [`support::clock`].
pub use support::clock::System as SystemClock;
pub use support::clock::{Manual as FakeClock, Mark, Miss};
