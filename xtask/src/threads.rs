//! Nextest thread count from host load (`docs/testing/running.md`,
//! "Running tests"): one thread per CPU the host has free, floored at a
//! small minimum so the gate keeps moving under extreme load.

/// Nextest `--test-threads` for `cpus` online CPUs under `load` (the
/// 1-minute average): `floor(cpus - load)`, at least 2, at most `cpus`.
/// The shell parser passes the raw counts; the floor happens here, where
/// the unit tests pin it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 1..=cpus, far below u64::MAX"
)]
pub(crate) fn threads(cpus: f64, load: f64) -> u64 {
    (cpus - load).floor().max(2.0).min(cpus.max(1.0)) as u64
}

#[cfg(test)]
#[path = "threads_tests.rs"]
mod tests;
