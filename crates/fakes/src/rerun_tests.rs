use super::*;

const CHILD: &str = "FIBER_FAKES_RERUN_CHILD";
const PROBE: &str = "FIBER_FAKES_RERUN_PROBE";
const HANG: &str = "FIBER_FAKES_RERUN_HANG";
const DIR: &str = "FIBER_FAKES_RERUN_DIR";
const PID_FILE: &str = "FIBER_FAKES_RERUN_PID_FILE";

/// The default `rerun` bound stays below the standard re-run caller wait
/// (30 s), so callers on the default need no per-child budget.
const _: () = assert!(CHILD_WITHIN.as_secs() < 30);

#[test]
#[allow(
    clippy::print_stdout,
    reason = "the parent test reads this line from the child's output"
)]
fn rerun_returns_the_child_output_with_its_extra_env() {
    if std::env::var_os(CHILD).is_some() {
        assert_eq!(std::env::var(PROBE).unwrap(), "yes");
        assert_eq!(std::env::var("HTTPS_PROXY").unwrap(), "http://127.0.0.1:9");
        for var in SCRUBBED {
            if var == "HTTPS_PROXY" {
                continue;
            }
            assert!(std::env::var_os(var).is_none(), "{var} reaches the child");
        }
        println!("rerun child ran");
        return;
    }
    let output = rerun(
        "rerun::tests::rerun_returns_the_child_output_with_its_extra_env",
        &[
            (CHILD, "1"),
            (PROBE, "yes"),
            ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ],
    );
    assert!(
        output.status.success(),
        "the re-run child ran:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("rerun child ran"),
        "the child's output comes back"
    );
}

#[test]
fn rerun_scrubs_the_eight_proxy_variables() {
    for var in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        assert!(SCRUBBED.contains(&var), "{var} is scrubbed");
    }
    assert_eq!(SCRUBBED.len(), 8);
}

#[test]
fn rerun_within_kills_a_child_that_outlives_the_callers_bound() {
    if std::env::var_os(HANG).is_some() {
        loop {
            std::thread::park();
        }
    }
    let bound = Duration::from_millis(300);
    let outcome = std::panic::catch_unwind(|| {
        rerun_within(
            "rerun::tests::rerun_within_kills_a_child_that_outlives_the_callers_bound",
            &[(HANG, "1")],
            Deadline::after(bound),
        )
    });
    let payload = outcome.expect_err("a child that never exits panics the caller");
    let message = payload
        .downcast_ref::<String>()
        .expect("the panic carries a formatted message");
    assert!(
        message.starts_with("waited ")
            && message.contains(
                "for `rerun::tests::rerun_within_kills_a_child_that_outlives_the_callers_bound` to exit",
            )
            && message.contains("reaped: true"),
        "the panic names the caller's bound and the reaped child: {message}"
    );
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "the parent test reads this line from the child's output"
)]
fn a_prepared_rerun_child_leads_its_own_group_in_the_prepared_directory() {
    const TEST: &str =
        "rerun::tests::a_prepared_rerun_child_leads_its_own_group_in_the_prepared_directory";
    if std::env::var_os(CHILD).is_some() {
        let pid = rustix::process::getpid();
        assert_eq!(
            rustix::process::getpgrp(),
            pid,
            "the child leads its own process group"
        );
        let dir = std::env::var(DIR).unwrap();
        assert_eq!(
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
            std::fs::canonicalize(&dir).unwrap(),
            "the child runs in the prepared directory"
        );
        println!("prepared rerun child ran");
        return;
    }
    let dir = crate::TempDir::new("fiber-rerun-prepared");
    let output = rerun_prepared(
        TEST,
        &[(CHILD, "1"), (DIR, dir.path().to_str().unwrap())],
        Deadline::after(Duration::from_secs(10)),
        |command| {
            command.current_dir(dir.path());
            command.process_group(rustix::process::getpgrp().as_raw_pid());
        },
    );
    assert!(
        output.status.success(),
        "the prepared child ran in its directory and group:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("prepared rerun child ran"),
        "the child's output comes back"
    );
}

#[test]
fn a_prepared_rerun_child_that_leaves_its_group_occupied_fails_and_the_group_is_killed() {
    const TEST: &str = "rerun::tests::a_prepared_rerun_child_that_leaves_its_group_occupied_fails_and_the_group_is_killed";
    if std::env::var_os(CHILD).is_some() {
        let mut sleeper = std::process::Command::new("sleep");
        sleeper
            .arg("3600")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = sleeper.spawn().unwrap();
        std::fs::write(std::env::var(PID_FILE).unwrap(), child.id().to_string()).unwrap();
        // The sleeper outlives this child on purpose: the rerun core kills
        // and reaps the group, so no handle here ever waits on it.
        std::mem::forget(child);
        return;
    }
    let dir = crate::TempDir::new("fiber-rerun-leftover");
    let pid_file = dir.path().join("sleeper.pid");
    // The group-empty wait is half of this, and the kill's own reap
    // needs the other half on a loaded host.
    const REAP: Duration = Duration::from_secs(4);
    let outcome = std::panic::catch_unwind(|| {
        rerun_bounded(
            TEST,
            &[(CHILD, "1"), (PID_FILE, pid_file.to_str().unwrap())],
            Duration::from_secs(10),
            REAP,
            |_| {},
        )
    });
    let payload = outcome.expect_err("a child that leaves its group occupied panics the caller");
    let message = payload
        .downcast_ref::<String>()
        .expect("the panic carries a formatted message");
    assert!(
        message.contains("left processes in its group")
            && message.contains(TEST)
            && message.contains("group empty: true"),
        "the panic names the leftover group and the test, and reports the group already empty: {message}"
    );
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // A zombie reparented to init reads alive until init reaps it, so the
    // bound absorbs the reap; an unkilled sleeper (3600 s) still fails it.
    assert!(
        crate::pids_exit(&[pid], Deadline::after(crate::MUST_SUCCEED_WITHIN)),
        "the group was killed before the panic"
    );
}

/// A re-run child drops the crash exception port that ReportCrash listens on.
#[cfg(target_os = "macos")]
#[test]
#[allow(
    clippy::print_stdout,
    reason = "the parent test reads this line from the child's output"
)]
fn a_rerun_child_has_no_crash_exception_port() {
    const TEST: &str = "rerun::tests::a_rerun_child_has_no_crash_exception_port";
    if std::env::var_os(CHILD).is_some() {
        println!("crash ports: {}", crate::crash_ports::probe::handlers());
        return;
    }
    let output = rerun_prepared(
        TEST,
        &[(CHILD, "1")],
        Deadline::after(Duration::from_secs(10)),
        crate::crash_ports::probe::inherit_a_live_port,
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("crash ports: 0"),
        "the child still has a crash exception port:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn half_of_the_cleanup_bound_is_half() {
    assert_eq!(half(Duration::from_secs(10)), Duration::from_secs(5));
    assert_eq!(half(Duration::ZERO), Duration::ZERO);
}
