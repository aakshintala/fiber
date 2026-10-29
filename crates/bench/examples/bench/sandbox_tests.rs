use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;

use super::{WORKLOADS, fixture, time_prepare};
use crate::home::Home;
use crate::idle::Ctx;

/// A `Ctx` over `home` with a parked fake clock: the workload never
/// sleeps, so the clock stays parked and only stamps the window.
fn ctx<'a>(home: &'a Home, clock: &'a dyn Clock) -> Ctx<'a> {
    Ctx {
        home,
        clock,
        idle: Duration::ZERO,
        path: None,
        runs: 1,
        paging: None,
    }
}

/// A home built from the test binary: the workload never spawns from
/// it, so the copy is only a path the fixture never reads.
fn home() -> Home {
    let exe = std::env::current_exe().expect("the test binary has a path");
    Home::new(&exe).expect("a home copies the test binary")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn sandbox_setup_reports_one_timed_sample() {
    assert_eq!(WORKLOADS.len(), 1);
    let workload = &WORKLOADS[0];
    assert_eq!(workload.name, "sandbox setup");
    assert!(workload.timing);
    let home = home();
    let clock = FakeClock::new();
    let samples = (workload.run)(&ctx(&home, &*clock), &mut Vec::new()).expect("prepare succeeds");
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].0, "sandbox_setup_ms");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_deny_inside_the_workspace_fails_the_sample() {
    let fixture = fixture().expect("the fixture builds");
    let mut denied = fixture.settings.clone();
    denied.deny_read.push(fixture.workspace.clone());
    let clock = FakeClock::new();
    assert!(
        time_prepare(&*clock, &denied, &fixture.workspace).is_err(),
        "a deny inside the workspace is unavailable"
    );
}
