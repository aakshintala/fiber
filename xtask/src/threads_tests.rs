//! Thread-count pins (`docs/testing/running.md:22`): `scripts/check` runs
//! nextest with one thread per CPU the host has free, so each count below is
//! `floor(cpus - load)`, floored at 2 and capped at `cpus`.

use super::*;

#[test]
fn eight_cpus_at_four_and_a_half_load_runs_three() {
    assert_eq!(threads(8.0, 4.5), 3);
}

#[test]
fn load_past_the_cpu_count_floors_at_two() {
    assert_eq!(threads(4.0, 10.0), 2);
}

#[test]
fn a_nearly_idle_host_runs_almost_every_cpu() {
    assert_eq!(threads(16.0, 0.5), 15);
}

#[test]
fn two_idle_cpus_run_two() {
    assert_eq!(threads(2.0, 0.0), 2);
}

#[test]
fn one_loaded_cpu_still_runs_one() {
    assert_eq!(threads(1.0, 0.9), 1);
}

#[test]
fn a_negative_load_clamps_at_the_cpu_count() {
    assert_eq!(threads(4.0, -1.0), 4);
}

#[test]
fn load_exactly_covering_the_cpus_floors_at_two() {
    assert_eq!(threads(8.0, 8.0), 2);
}
