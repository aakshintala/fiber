use super::*;

const CHILD: &str = "FIBER_FAKES_RERUN_CHILD";
const PROBE: &str = "FIBER_FAKES_RERUN_PROBE";
const HANG: &str = "FIBER_FAKES_RERUN_HANG";

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
            bound,
        )
    });
    let payload = outcome.expect_err("a child that never exits panics the caller");
    let message = payload
        .downcast_ref::<String>()
        .expect("the panic carries a formatted message");
    assert!(
        message.contains("waited 300ms") && message.contains("reaped: true"),
        "the panic names the caller's bound and the reaped child: {message}"
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
        Duration::from_secs(10),
        crate::crash_ports::probe::inherit_a_live_port,
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("crash ports: 0"),
        "the child still has a crash exception port:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
